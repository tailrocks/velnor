//! Mutual TLS material and bridge for the per-job Docker API lease.
//!
//! Each lease gets a new EC certificate authority. The client bundle is stored
//! under the runner-owned, read-only BuildKit config mount; the server key
//! stays in memory on the runner host. Client files are readable by any UID
//! inside that per-job mount because the image `USER` and admitted `--user`
//! option may select arbitrary numeric identities. Their host source stays
//! beneath a runner-owned owner-only control directory.

use anyhow::{anyhow, bail, Context, Result};
use openssl::{
    asn1::{Asn1Integer, Asn1Time},
    bn::{BigNum, MsbOption},
    ec::{EcGroup, EcKey},
    hash::MessageDigest,
    nid::Nid,
    pkey::{PKey, Private},
    x509::{
        extension::{
            AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage,
            SubjectAlternativeName, SubjectKeyIdentifier,
        },
        X509Builder, X509Name, X509NameBuilder, X509,
    },
};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName},
    server::WebPkiClientVerifier,
    RootCertStore, ServerConfig,
};
use std::{
    ffi::OsString,
    fs::{self, File},
    io::Write,
    net::TcpStream as StdTcpStream,
    os::unix::net::UnixStream as StdUnixStream,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::copy_bidirectional,
    net::{TcpStream, UnixStream},
    runtime::Builder,
    time::timeout,
};
use tokio_rustls::{rustls, TlsAcceptor};

const CA_FILE: &str = "ca.pem";
const CLIENT_CERT_FILE: &str = "cert.pem";
const CLIENT_KEY_FILE: &str = "key.pem";
// Other container UIDs can enter the directory to open Docker's known names,
// but cannot list it. The config mount itself remains read-only.
const TLS_BUNDLE_DIR_MODE: u16 = 0o711;
const TLS_BUNDLE_FILE_MODE: u16 = 0o444;

/// Path visible inside the job container through its runner-owned read-only
/// BuildKit config mount. TLS material must never live under the job-writable
/// `/__t` tree.
pub const DOCKER_TLS_GUEST_CERT_DIR: &str = "/__velnor-buildkit-configs/docker-tls";

const CERT_VALIDITY_DAYS: u32 = 1;
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
// Job cleanup normally closes the tracked TCP sockets much sooner. Keep an
// upper bound for abandoned bridge workers if cleanup cannot close a socket.
const TLS_BRIDGE_MAX_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

/// Owns cleanup for one per-lease TLS bundle.
///
/// Callers must retain this handle for the lease lifetime. Dropping it removes
/// only the three known PEM entries through the captured directory handles,
/// after confirming that the original parent and bundle are still anchored at
/// their creation paths. Replacement paths or unexpected entries fail closed.
pub(crate) struct LeaseTlsBundleCleanup {
    parent_path: PathBuf,
    leaf_name: OsString,
    parent: File,
    directory: File,
    parent_identity: DirectoryIdentity,
    directory_identity: DirectoryIdentity,
    cleaned: bool,
}

impl LeaseTlsBundleCleanup {
    fn remove_bundle(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }

        self.verify_parent_anchor()?;
        self.verify_leaf_anchor()?;

        let entries = self.known_entries()?;
        for name in entries {
            self.verify_regular_entry(name)?;
            rustix::fs::unlinkat(
                &self.directory,
                Path::new(name),
                rustix::fs::AtFlags::empty(),
            )
            .map_err(std::io::Error::from)
            .with_context(|| format!("removing Docker TLS bundle entry {name}"))?;
        }

        self.verify_parent_anchor()?;
        self.verify_leaf_anchor()?;
        self.verify_empty_directory()?;
        rustix::fs::unlinkat(
            &self.parent,
            Path::new(&self.leaf_name),
            rustix::fs::AtFlags::REMOVEDIR,
        )
        .map_err(std::io::Error::from)
        .context("removing per-lease Docker TLS bundle directory")?;
        self.cleaned = true;
        Ok(())
    }

    fn verify_parent_anchor(&self) -> Result<()> {
        if directory_identity(&self.parent)? != self.parent_identity {
            bail!("Docker TLS bundle parent handle identity changed");
        }

        let path_fd = rustix::fs::openat(
            rustix::fs::CWD,
            &self.parent_path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)
        .context("reopening Docker TLS bundle parent without following symlinks")?;
        let path_directory: File = path_fd.into();
        if directory_identity(&path_directory)? != self.parent_identity {
            bail!("Docker TLS bundle parent path identity changed");
        }
        Ok(())
    }

    fn verify_leaf_anchor(&self) -> Result<()> {
        if directory_identity(&self.directory)? != self.directory_identity {
            bail!("Docker TLS bundle directory handle identity changed");
        }

        let stat = rustix::fs::statat(
            &self.parent,
            Path::new(&self.leaf_name),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(std::io::Error::from)
        .context("inspecting Docker TLS bundle directory without following symlinks")?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Directory
            || identity_from_stat(&stat) != self.directory_identity
        {
            bail!("Docker TLS bundle directory path identity changed");
        }
        Ok(())
    }

    fn known_entries(&self) -> Result<Vec<&'static str>> {
        let entries = rustix::fs::Dir::read_from(&self.directory)
            .map_err(std::io::Error::from)
            .context("reading Docker TLS bundle directory")?;
        let mut found = [false; 3];
        for entry in entries {
            let entry = entry.map_err(std::io::Error::from)?;
            match entry.file_name().to_bytes() {
                b"." | b".." => {}
                name if name == CA_FILE.as_bytes() => found[0] = true,
                name if name == CLIENT_CERT_FILE.as_bytes() => found[1] = true,
                name if name == CLIENT_KEY_FILE.as_bytes() => found[2] = true,
                _ => bail!("unexpected entry in per-lease Docker TLS bundle"),
            }
        }
        let mut names = Vec::with_capacity(3);
        if found[0] {
            names.push(CA_FILE);
        }
        if found[1] {
            names.push(CLIENT_CERT_FILE);
        }
        if found[2] {
            names.push(CLIENT_KEY_FILE);
        }
        Ok(names)
    }

    fn verify_regular_entry(&self, name: &str) -> Result<()> {
        let fd = rustix::fs::openat(
            &self.directory,
            Path::new(name),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)
        .with_context(|| {
            format!("opening Docker TLS bundle entry {name} without following symlinks")
        })?;
        let stat = rustix::fs::fstat(&fd).map_err(std::io::Error::from)?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
            bail!("Docker TLS bundle entry {name} is not a regular file");
        }
        Ok(())
    }

    fn verify_empty_directory(&self) -> Result<()> {
        let entries = rustix::fs::Dir::read_from(&self.directory)
            .map_err(std::io::Error::from)
            .context("checking Docker TLS bundle directory before removal")?;
        for entry in entries {
            let entry = entry.map_err(std::io::Error::from)?;
            match entry.file_name().to_bytes() {
                b"." | b".." => {}
                _ => bail!("Docker TLS bundle directory changed during cleanup"),
            }
        }
        Ok(())
    }
}

impl Drop for LeaseTlsBundleCleanup {
    fn drop(&mut self) {
        if let Err(error) = self.remove_bundle() {
            tracing::warn!(%error, "skipping unsafe Docker TLS bundle cleanup");
        }
    }
}

struct LeaseCertificates {
    ca_certificate: X509,
    server_certificate: X509,
    server_key: PKey<Private>,
    client_certificate: X509,
    client_key: PKey<Private>,
}

/// Create a unique mTLS CA and Docker client bundle for one job lease.
///
/// The bundle is readable by any container UID so both the image `USER` and
/// admitted `--user` option work without resolving container identity here.
/// Its source must stay below a runner-owned owner-only host directory; the
/// container receives it only through the job's read-only BuildKit config
/// bind mount. The returned server configuration trusts only the client
/// certificate issued by this call's private CA. The guest path is the fixed
/// location inside that mount. Callers must retain and drop the cleanup handle
/// with the lease; it owns identity-bound cleanup for the bundle.
pub fn create_lease_tls(
    cert_dir: &Path,
    hostname: &str,
) -> Result<(Arc<ServerConfig>, PathBuf, LeaseTlsBundleCleanup)> {
    validate_dns_hostname(hostname)?;

    let requested_parent = cert_dir
        .parent()
        .ok_or_else(|| anyhow!("Docker TLS certificate directory has no parent"))?;
    fs::create_dir_all(requested_parent).with_context(|| {
        format!(
            "creating Docker TLS certificate parent {}",
            requested_parent.display()
        )
    })?;

    let parent_path = absolute_path(requested_parent)?;
    let parent_fd = rustix::fs::openat(
        rustix::fs::CWD,
        &parent_path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)
    .with_context(|| {
        format!(
            "opening Docker TLS certificate parent without following symlinks: {}",
            parent_path.display()
        )
    })?;
    let parent: File = parent_fd.into();
    verify_private_host_ancestor(&parent)?;
    let parent_identity = directory_identity(&parent)?;

    let leaf_name = cert_dir
        .file_name()
        .ok_or_else(|| anyhow!("Docker TLS certificate directory has no final component"))?
        .to_os_string();
    rustix::fs::mkdirat(&parent, &leaf_name, rustix::fs::Mode::from_raw_mode(0o700))
        .map_err(std::io::Error::from)
        .with_context(|| {
            format!(
                "creating per-job Docker TLS certificate directory {}",
                cert_dir.display()
            )
        })?;

    let directory_fd = rustix::fs::openat(
        &parent,
        &leaf_name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)
    .with_context(|| {
        format!(
            "opening per-job Docker TLS certificate directory without following symlinks: {}",
            cert_dir.display()
        )
    })?;
    let directory: File = directory_fd.into();
    let directory_identity = directory_identity(&directory)?;

    let cleanup = LeaseTlsBundleCleanup {
        parent_path,
        leaf_name,
        parent,
        directory,
        parent_identity,
        directory_identity,
        cleaned: false,
    };
    let (server_config, guest_cert_dir) = create_lease_tls_in_dir(&cleanup, hostname)?;
    rustix::fs::fchmod(
        &cleanup.directory,
        rustix::fs::Mode::from_raw_mode(TLS_BUNDLE_DIR_MODE),
    )
    .map_err(std::io::Error::from)
    .context("making per-job Docker TLS certificate directory readable")?;
    Ok((server_config, guest_cert_dir, cleanup))
}

fn create_lease_tls_in_dir(
    cleanup: &LeaseTlsBundleCleanup,
    hostname: &str,
) -> Result<(Arc<ServerConfig>, PathBuf)> {
    let certificates = generate_certificates(hostname)?;
    write_guest_readable_file(
        &cleanup.directory,
        CA_FILE,
        &certificates.ca_certificate.to_pem()?,
    )?;
    write_guest_readable_file(
        &cleanup.directory,
        CLIENT_CERT_FILE,
        &certificates.client_certificate.to_pem()?,
    )?;
    write_guest_readable_file(
        &cleanup.directory,
        CLIENT_KEY_FILE,
        &certificates.client_key.private_key_to_pem_pkcs8()?,
    )?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from(certificates.ca_certificate.to_der()?))?;
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .context("building per-job Docker client certificate verifier")?;

    let server_certificate = CertificateDer::from(certificates.server_certificate.to_der()?);
    let server_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certificates.server_key.private_key_to_pkcs8()?,
    ));
    let server_config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("selecting Docker lease TLS protocol versions")?
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![server_certificate], server_key)
        .context("building per-job Docker lease TLS server configuration")?;

    Ok((
        Arc::new(server_config),
        PathBuf::from(DOCKER_TLS_GUEST_CERT_DIR),
    ))
}

fn validate_dns_hostname(hostname: &str) -> Result<()> {
    let parsed = ServerName::try_from(hostname.to_owned())
        .map_err(|_| anyhow!("invalid Docker lease TLS DNS hostname"))?;
    if !matches!(parsed, ServerName::DnsName(_)) {
        bail!("Docker lease TLS hostname must be a DNS name");
    }
    Ok(())
}

fn write_guest_readable_file(directory: &File, name: &str, contents: &[u8]) -> Result<()> {
    let mut file: File = rustix::fs::openat(
        directory,
        Path::new(name),
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .map_err(std::io::Error::from)
    .with_context(|| format!("creating Docker TLS file {name}"))?
    .into();
    file.write_all(contents)
        .with_context(|| format!("writing private Docker TLS file {name}"))?;
    rustix::fs::fchmod(&file, rustix::fs::Mode::from_raw_mode(TLS_BUNDLE_FILE_MODE))
        .map_err(std::io::Error::from)
        .with_context(|| format!("making Docker TLS file {name} readable in the job container"))?;
    file.sync_all()
        .with_context(|| format!("syncing private Docker TLS file {name}"))?;
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn directory_identity(directory: &File) -> Result<DirectoryIdentity> {
    let stat = rustix::fs::fstat(directory).map_err(std::io::Error::from)?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Directory {
        bail!("Docker TLS bundle anchor is not a directory");
    }
    Ok(identity_from_stat(&stat))
}

/// Require a runner-owned host directory with no group/other access somewhere
/// above the mount source. This keeps the world-readable modes inside the
/// bind mount from making the key host-readable. Walk using open directory
/// handles so symlinked path components cannot fabricate the privacy check.
fn verify_private_host_ancestor(parent: &File) -> Result<()> {
    let runner_uid = rustix::process::geteuid().as_raw();
    let mut ancestor = parent
        .try_clone()
        .context("cloning Docker TLS host parent directory handle")?;

    loop {
        let stat = rustix::fs::fstat(&ancestor).map_err(std::io::Error::from)?;
        if stat.st_uid == runner_uid && stat.st_mode & 0o077 == 0 {
            return Ok(());
        }

        let parent_fd = rustix::fs::openat(
            &ancestor,
            Path::new(".."),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)
        .context("walking Docker TLS host parent directories")?;
        let next: File = parent_fd.into();
        if directory_identity(&next)? == directory_identity(&ancestor)? {
            bail!("Docker TLS bundle has no runner-owned owner-only host ancestor");
        }
        ancestor = next;
    }
}

fn identity_from_stat(stat: &rustix::fs::Stat) -> DirectoryIdentity {
    DirectoryIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
    }
}

fn generate_certificates(hostname: &str) -> Result<LeaseCertificates> {
    let ca_key = generate_p256_key()?;
    let ca_certificate = generate_ca_certificate(&ca_key)?;

    let server_key = generate_p256_key()?;
    let server_certificate = generate_leaf_certificate(
        "velnor Docker lease server",
        &server_key,
        &ca_certificate,
        &ca_key,
        LeafUsage::Server { hostname },
    )?;

    let client_key = generate_p256_key()?;
    let client_certificate = generate_leaf_certificate(
        "velnor Docker lease client",
        &client_key,
        &ca_certificate,
        &ca_key,
        LeafUsage::Client,
    )?;

    Ok(LeaseCertificates {
        ca_certificate,
        server_certificate,
        server_key,
        client_certificate,
        client_key,
    })
}

fn generate_p256_key() -> Result<PKey<Private>> {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1)?;
    let key = EcKey::generate(&group)?;
    Ok(PKey::from_ec_key(key)?)
}

fn generate_serial() -> Result<Asn1Integer> {
    let mut value = BigNum::new()?;
    value.rand(128, MsbOption::ONE, true)?;
    Ok(Asn1Integer::from_bn(&value)?)
}

fn x509_name(common_name: &str) -> Result<X509Name> {
    let mut name = X509NameBuilder::new()?;
    name.append_entry_by_text("CN", common_name)?;
    Ok(name.build())
}

fn generate_ca_certificate(ca_key: &PKey<Private>) -> Result<X509> {
    let name = x509_name("velnor per-job Docker lease CA")?;
    let mut builder = X509::builder()?;
    builder.set_version(2)?;
    let serial = generate_serial()?;
    builder.set_serial_number(&serial)?;
    builder.set_subject_name(&name)?;
    builder.set_issuer_name(&name)?;
    builder.set_pubkey(ca_key)?;
    set_validity(&mut builder)?;
    builder.append_extension(BasicConstraints::new().critical().ca().build()?)?;
    builder.append_extension(
        KeyUsage::new()
            .critical()
            .key_cert_sign()
            .crl_sign()
            .build()?,
    )?;
    {
        let context = builder.x509v3_context(None, None);
        builder.append_extension(SubjectKeyIdentifier::new().build(&context)?)?;
    }
    builder.sign(ca_key, MessageDigest::sha256())?;
    Ok(builder.build())
}

enum LeafUsage<'a> {
    Server { hostname: &'a str },
    Client,
}

fn generate_leaf_certificate(
    common_name: &str,
    key: &PKey<Private>,
    ca_certificate: &X509,
    ca_key: &PKey<Private>,
    usage: LeafUsage<'_>,
) -> Result<X509> {
    let subject = x509_name(common_name)?;
    let mut builder = X509::builder()?;
    builder.set_version(2)?;
    let serial = generate_serial()?;
    builder.set_serial_number(&serial)?;
    builder.set_subject_name(&subject)?;
    builder.set_issuer_name(ca_certificate.subject_name())?;
    builder.set_pubkey(key)?;
    set_validity(&mut builder)?;
    builder.append_extension(BasicConstraints::new().critical().build()?)?;
    builder.append_extension(KeyUsage::new().critical().digital_signature().build()?)?;

    match usage {
        LeafUsage::Server { hostname } => {
            builder.append_extension(ExtendedKeyUsage::new().server_auth().build()?)?;
            let context = builder.x509v3_context(Some(ca_certificate), None);
            builder.append_extension(
                SubjectAlternativeName::new()
                    .dns(hostname)
                    .build(&context)?,
            )?;
        }
        LeafUsage::Client => {
            builder.append_extension(ExtendedKeyUsage::new().client_auth().build()?)?;
        }
    }
    {
        let context = builder.x509v3_context(Some(ca_certificate), None);
        builder.append_extension(SubjectKeyIdentifier::new().build(&context)?)?;
    }
    {
        let context = builder.x509v3_context(Some(ca_certificate), None);
        builder.append_extension(
            AuthorityKeyIdentifier::new()
                .keyid(true)
                .issuer(true)
                .build(&context)?,
        )?;
    }
    builder.sign(ca_key, MessageDigest::sha256())?;
    Ok(builder.build())
}

fn set_validity(builder: &mut X509Builder) -> Result<()> {
    let not_before = Asn1Time::days_from_now(0)?;
    let not_after = Asn1Time::days_from_now(CERT_VALIDITY_DAYS)?;
    builder.set_not_before(&not_before)?;
    builder.set_not_after(&not_after)?;
    Ok(())
}

/// Bridge one authenticated TCP lease to the daemon Unix socket.
///
/// The function owns both streams and returns after they close or the 24-hour
/// maximum lifetime expires.
pub fn bridge_tls_tcp_to_unix(
    tcp: StdTcpStream,
    config: Arc<ServerConfig>,
    unix: StdUnixStream,
) -> Result<()> {
    tcp.set_nonblocking(true)
        .context("setting Docker lease TCP stream nonblocking")?;
    tcp.set_nodelay(true)
        .context("enabling TCP_NODELAY for Docker lease")?;
    unix.set_nonblocking(true)
        .context("setting Docker lease Unix stream nonblocking")?;

    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building Docker lease TLS bridge runtime")?;
    let acceptor = TlsAcceptor::from(config);
    runtime.block_on(async move {
        let tcp = TcpStream::from_std(tcp).context("registering Docker lease TCP stream")?;
        let mut unix =
            UnixStream::from_std(unix).context("registering Docker daemon Unix stream")?;
        let mut tls = timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(tcp))
            .await
            .context("Docker lease TLS handshake timed out")?
            .context("Docker lease TLS client certificate was rejected")?;
        timeout(
            TLS_BRIDGE_MAX_LIFETIME,
            copy_bidirectional(&mut tls, &mut unix),
        )
        .await
        .context("Docker lease TLS bridge exceeded its maximum lifetime")?
        .context("copying Docker lease traffic")?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::rustls::{
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName},
        ClientConfig, RootCertStore,
    };
    use super::{
        bridge_tls_tcp_to_unix, create_lease_tls, rustls, CA_FILE, CLIENT_CERT_FILE,
        CLIENT_KEY_FILE, DOCKER_TLS_GUEST_CERT_DIR, TLS_BUNDLE_DIR_MODE, TLS_BUNDLE_FILE_MODE,
    };
    use anyhow::{anyhow, Context, Result};
    use openssl::{pkey::PKey, x509::X509};
    use std::{
        fs,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        os::unix::{
            fs::{MetadataExt, PermissionsExt},
            net::UnixStream,
        },
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream as TokioTcpStream,
        time::timeout,
    };
    use tokio_rustls::TlsConnector;

    struct TempDirectory(PathBuf);

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct TlsTestMount {
        _temporary_directory: TempDirectory,
        control_dir: PathBuf,
        config_mount: PathBuf,
        cert_dir: PathBuf,
    }

    fn tls_test_mount(name: &str) -> Result<TlsTestMount> {
        let root = std::env::temp_dir().join(format!(
            "velnor-docker-lease-tls-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&root)?;
        let temporary_directory = TempDirectory(root.clone());

        let control_dir = root.join("control");
        fs::create_dir(&control_dir)?;
        fs::set_permissions(&control_dir, fs::Permissions::from_mode(0o700))?;

        let config_mount = control_dir.join("buildkit-configs");
        fs::create_dir(&config_mount)?;
        fs::set_permissions(&config_mount, fs::Permissions::from_mode(0o755))?;

        Ok(TlsTestMount {
            _temporary_directory: temporary_directory,
            control_dir,
            cert_dir: config_mount.join("docker-tls"),
            config_mount,
        })
    }

    fn mode_grants(metadata: &fs::Metadata, uid: u32, gid: u32, requested: u32) -> bool {
        let shift = if uid == metadata.uid() {
            6
        } else if gid == metadata.gid() {
            3
        } else {
            0
        };
        (metadata.mode() >> shift) & requested == requested
    }

    fn other_id(owners: &[u32]) -> u32 {
        (1..u32::MAX)
            .find(|candidate| !owners.contains(candidate))
            .expect("a non-owner identity is available")
    }

    fn client_config(cert_dir: &Path, include_client_cert: bool) -> Result<Arc<ClientConfig>> {
        let ca_certificate = X509::from_pem(&fs::read(cert_dir.join("ca.pem"))?)?;
        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from(ca_certificate.to_der()?))?;

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .context("selecting test client TLS protocol versions")?
            .with_root_certificates(roots);
        let config = if include_client_cert {
            let client_certificate = X509::from_pem(&fs::read(cert_dir.join("cert.pem"))?)?;
            let client_key = PKey::private_key_from_pem(&fs::read(cert_dir.join("key.pem"))?)?;
            let key =
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(client_key.private_key_to_pkcs8()?));
            builder
                .with_client_auth_cert(
                    vec![CertificateDer::from(client_certificate.to_der()?)],
                    key,
                )
                .context("building authenticated test Docker client")?
        } else {
            builder.with_no_client_auth()
        };
        Ok(Arc::new(config))
    }

    async fn try_echo_through_bridge(
        client_config: Arc<ClientConfig>,
        server_config: Arc<rustls::ServerConfig>,
    ) -> Result<bool> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let client_tcp = TcpStream::connect(address)?;
        let (server_tcp, _) = listener.accept()?;
        let (bridge_unix, mut upstream_unix) = UnixStream::pair()?;

        let bridge_thread = std::thread::spawn(move || {
            bridge_tls_tcp_to_unix(server_tcp, server_config, bridge_unix)
        });
        let upstream_thread = std::thread::spawn(move || -> std::io::Result<()> {
            let mut request = [0; 4];
            upstream_unix.read_exact(&mut request)?;
            if request != *b"PING" {
                return Err(std::io::Error::other("unexpected upstream test request"));
            }
            upstream_unix.write_all(b"PONG")?;
            let mut trailing = [0; 1];
            if upstream_unix.read(&mut trailing)? == 0 {
                Ok(())
            } else {
                Err(std::io::Error::other(
                    "unexpected trailing authenticated test request",
                ))
            }
        });

        client_tcp.set_nonblocking(true)?;
        let client_tcp = TokioTcpStream::from_std(client_tcp)?;
        let server_name = ServerName::try_from("host.docker.internal".to_owned())
            .map_err(|_| anyhow!("building test Docker lease server name"))?;
        let connector = TlsConnector::from(client_config);
        let connected = timeout(
            Duration::from_secs(3),
            connector.connect(server_name, client_tcp),
        )
        .await;

        let succeeded = if let Ok(Ok(mut tls)) = connected {
            let echo = timeout(Duration::from_secs(3), async {
                tls.write_all(b"PING").await?;
                tls.flush().await?;
                let mut response = [0; 4];
                tls.read_exact(&mut response).await?;
                let matched = response == *b"PONG";
                tls.shutdown().await?;
                Ok::<bool, std::io::Error>(matched)
            })
            .await;
            matches!(echo, Ok(Ok(true)))
        } else {
            false
        };

        let upstream_result = upstream_thread
            .join()
            .map_err(|_| anyhow!("Docker TLS test upstream thread panicked"))?;
        let bridge_result = bridge_thread
            .join()
            .map_err(|_| anyhow!("Docker TLS test bridge thread panicked"))?;
        if succeeded {
            upstream_result.context("authenticated test request did not reach upstream")?;
            bridge_result.context("authenticated test bridge failed")?;
        } else if bridge_result.is_ok() {
            return Err(anyhow!(
                "Docker TLS bridge accepted a client without a valid client certificate"
            ));
        }
        Ok(succeeded)
    }

    #[tokio::test]
    async fn lease_tls_is_readable_through_job_mount_and_requires_client_certificate() -> Result<()>
    {
        let mount = tls_test_mount("client")?;
        let cert_dir = &mount.cert_dir;
        let (server_config, guest_path, cleanup) =
            create_lease_tls(cert_dir, "host.docker.internal")?;

        assert_eq!(guest_path, PathBuf::from(DOCKER_TLS_GUEST_CERT_DIR));
        assert_eq!(
            Path::new(DOCKER_TLS_GUEST_CERT_DIR).parent(),
            Some(Path::new("/__velnor-buildkit-configs"))
        );
        assert_eq!(
            Path::new(DOCKER_TLS_GUEST_CERT_DIR)
                .file_name()
                .and_then(std::ffi::OsStr::to_str),
            Some("docker-tls")
        );
        assert_eq!(cert_dir.parent(), Some(mount.config_mount.as_path()));
        assert_eq!(
            fs::metadata(&mount.control_dir)?.permissions().mode() & 0o777,
            0o700,
            "the host source must stay beneath the private per-job control directory"
        );
        assert_eq!(
            fs::metadata(&mount.config_mount)?.permissions().mode() & 0o777,
            0o755,
            "the mounted config root must be traversable by arbitrary container UIDs"
        );
        let bundle_metadata = fs::metadata(cert_dir)?;
        assert_eq!(
            bundle_metadata.permissions().mode() & 0o777,
            u32::from(TLS_BUNDLE_DIR_MODE)
        );

        let control_metadata = fs::metadata(&mount.control_dir)?;
        let config_metadata = fs::metadata(&mount.config_mount)?;
        let owner_uids = [
            control_metadata.uid(),
            config_metadata.uid(),
            bundle_metadata.uid(),
        ];
        let owner_gids = [
            control_metadata.gid(),
            config_metadata.gid(),
            bundle_metadata.gid(),
        ];
        let container_uid = other_id(&owner_uids);
        let container_gid = other_id(&owner_gids);
        assert_ne!(container_uid, 0, "test identity must be non-root");
        assert!(
            !mode_grants(&control_metadata, container_uid, container_gid, 0o1),
            "the same UID cannot traverse the host-only control directory"
        );
        assert!(mode_grants(
            &config_metadata,
            container_uid,
            container_gid,
            0o1
        ));
        assert!(mode_grants(
            &bundle_metadata,
            container_uid,
            container_gid,
            0o1
        ));

        for name in ["ca.pem", "cert.pem", "key.pem"] {
            let file_metadata = fs::metadata(cert_dir.join(name))?;
            assert_eq!(
                file_metadata.permissions().mode() & 0o777,
                u32::from(TLS_BUNDLE_FILE_MODE),
                "{name} must be readable and immutable through the read-only job mount"
            );
            assert_eq!(file_metadata.uid(), bundle_metadata.uid());
            assert_eq!(file_metadata.gid(), bundle_metadata.gid());
            assert_ne!(file_metadata.uid(), container_uid);
            assert_ne!(file_metadata.gid(), container_gid);
            assert!(
                mode_grants(&file_metadata, container_uid, container_gid, 0o4),
                "non-owner container UID must read the mounted {name} through other bits"
            );
        }

        let authenticated_client = client_config(cert_dir, true)?;
        assert!(try_echo_through_bridge(authenticated_client, server_config.clone()).await?);

        let anonymous_client = client_config(cert_dir, false)?;
        assert!(!try_echo_through_bridge(anonymous_client, server_config).await?);
        drop(cleanup);
        assert!(!cert_dir.exists());
        Ok(())
    }

    #[test]
    fn tls_bundle_refuses_host_source_without_private_ancestor() -> Result<()> {
        let public_temp = fs::canonicalize("/tmp")?;
        let root = public_temp.join(format!(
            "velnor-docker-lease-tls-unprotected-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&root)?;
        let _temporary_directory = TempDirectory(root.clone());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755))?;

        let config_mount = root.join("buildkit-configs");
        fs::create_dir(&config_mount)?;
        fs::set_permissions(&config_mount, fs::Permissions::from_mode(0o755))?;
        let cert_dir = config_mount.join("docker-tls");
        let error = match create_lease_tls(&cert_dir, "host.docker.internal") {
            Ok((_, _, cleanup)) => {
                drop(cleanup);
                return Err(anyhow!("unprotected Docker TLS host source was accepted"));
            }
            Err(error) => error,
        };

        assert!(
            format!("{error:#}").contains("no runner-owned owner-only host ancestor"),
            "unexpected error: {error:#}"
        );
        assert!(!cert_dir.exists());
        Ok(())
    }

    #[test]
    fn tls_cleanup_does_not_follow_replaced_parent_symlink() -> Result<()> {
        let mount = tls_test_mount("parent-replacement")?;
        let parent = &mount.config_mount;
        let cert_dir = &mount.cert_dir;
        let (_, _, cleanup) = create_lease_tls(cert_dir, "host.docker.internal")?;
        let renamed_parent = mount.control_dir.join("renamed-buildkit-configs");
        let original_bundle = renamed_parent.join("docker-tls");
        let original_contents = [CA_FILE, CLIENT_CERT_FILE, CLIENT_KEY_FILE]
            .map(|name| fs::read(cert_dir.join(name)))
            .into_iter()
            .collect::<std::io::Result<Vec<_>>>()?;

        let outside = mount.control_dir.join("outside");
        let outside_bundle = outside.join("docker-tls");
        fs::create_dir_all(&outside_bundle)?;
        let outside_contents = [CA_FILE, CLIENT_CERT_FILE, CLIENT_KEY_FILE, "sentinel"]
            .map(|name| {
                let content = format!("outside:{name}").into_bytes();
                fs::write(outside_bundle.join(name), &content)?;
                Ok::<_, std::io::Error>(content)
            })
            .into_iter()
            .collect::<std::io::Result<Vec<_>>>()?;

        fs::rename(parent, &renamed_parent)?;
        std::os::unix::fs::symlink(&outside, parent)?;
        drop(cleanup);

        for (name, contents) in [CA_FILE, CLIENT_CERT_FILE, CLIENT_KEY_FILE]
            .into_iter()
            .zip(original_contents)
        {
            assert_eq!(fs::read(original_bundle.join(name))?, contents);
        }
        for (name, contents) in [CA_FILE, CLIENT_CERT_FILE, CLIENT_KEY_FILE, "sentinel"]
            .into_iter()
            .zip(outside_contents)
        {
            assert_eq!(fs::read(outside_bundle.join(name))?, contents);
        }
        Ok(())
    }

    #[test]
    fn tls_cleanup_fails_closed_on_extra_bundle_entry() -> Result<()> {
        let mount = tls_test_mount("extra-entry")?;
        let cert_dir = &mount.cert_dir;
        let (_, _, cleanup) = create_lease_tls(cert_dir, "host.docker.internal")?;
        let extra = cert_dir.join("unexpected");
        fs::write(&extra, "preserve")?;

        drop(cleanup);

        assert!(extra.is_file());
        for name in [CA_FILE, CLIENT_CERT_FILE, CLIENT_KEY_FILE] {
            assert!(cert_dir.join(name).is_file());
        }
        Ok(())
    }

    #[test]
    fn tls_cleanup_fails_closed_when_leaf_directory_is_replaced() -> Result<()> {
        let mount = tls_test_mount("leaf-replacement")?;
        let parent = &mount.config_mount;
        let cert_dir = &mount.cert_dir;
        let (_, _, cleanup) = create_lease_tls(cert_dir, "host.docker.internal")?;
        let original_bundle = parent.join("renamed-docker-tls");
        let original_contents = [CA_FILE, CLIENT_CERT_FILE, CLIENT_KEY_FILE]
            .map(|name| fs::read(cert_dir.join(name)))
            .into_iter()
            .collect::<std::io::Result<Vec<_>>>()?;

        fs::rename(&cert_dir, &original_bundle)?;
        fs::create_dir(&cert_dir)?;
        let replacement_contents = [CA_FILE, CLIENT_CERT_FILE, CLIENT_KEY_FILE]
            .map(|name| {
                let content = format!("replacement:{name}").into_bytes();
                fs::write(cert_dir.join(name), &content)?;
                Ok::<_, std::io::Error>(content)
            })
            .into_iter()
            .collect::<std::io::Result<Vec<_>>>()?;

        drop(cleanup);

        for (name, contents) in [CA_FILE, CLIENT_CERT_FILE, CLIENT_KEY_FILE]
            .into_iter()
            .zip(original_contents)
        {
            assert_eq!(fs::read(original_bundle.join(name))?, contents);
        }
        for (name, contents) in [CA_FILE, CLIENT_CERT_FILE, CLIENT_KEY_FILE]
            .into_iter()
            .zip(replacement_contents)
        {
            assert_eq!(fs::read(cert_dir.join(name))?, contents);
        }
        Ok(())
    }
}
