require "digest"
require "json"

# Product source is pinned; packaging assets are pinned separately until the
# formula is rendered into a tap. Never replace either pin with a branch or
# latest-release URL.
SOURCE_COMMIT = "e5a0c249157a6fa79e82b502843fef945030d225".freeze
SOURCE_ARCHIVE_SHA256 = "fb4dfd4a2e824d65ddebd597648c20d154a755bef23fd39afc71e5b134fe7dfa".freeze
PACKAGING_COMMIT = "cce2bc61d3d446797d857ca09202ade99c54615a".freeze
LAUNCH_SHA256 = "f025ac790fd3395e09c6fbb5c9e12e4db71eefc6d73c76371efa749414d65eaf".freeze
PLIST_SHA256 = "c7d3f69454e8d04a7913dfcfa0b7c0ea36b37c6339118cff580f1d4eb31493f5".freeze
SOURCE_URL = "https://github.com/tailrocks/velnor/archive/#{SOURCE_COMMIT}.tar.gz".freeze
GH_CLI_REQUIRED_FLAGS = %w[
  --repo
  --signer-workflow
  --predicate-type
  --cert-oidc-issuer
  --deny-self-hosted-runners
  --format
].freeze
GH_CLI_PREFLIGHT = %w[gh attestation verify --help].freeze
HOST_MODES = %w[native-only scale-set-only both].freeze
SCALE_HOST_MODES = %w[scale-set-only both].freeze
MACOS_QUALIFICATION_ORDER = %w[scale-set-only native-only both].freeze

class Velnorctl < Formula
  desc "Native Velnor operator, runner, and workflow toolset"
  homepage "https://github.com/tailrocks/velnor"
  url SOURCE_URL
  version "0.1.277"
  sha256 SOURCE_ARCHIVE_SHA256
  license "Apache-2.0"

  depends_on "rust" => :build
  depends_on "docker"
  depends_on "gh"
  depends_on "git"

  resource "velnor-runner-launch" do
    url "https://raw.githubusercontent.com/tailrocks/velnor/#{PACKAGING_COMMIT}/packaging/macos/velnor-runner-launch"
    sha256 LAUNCH_SHA256
  end

  resource "velnor-launchd-plist" do
    url "https://raw.githubusercontent.com/tailrocks/velnor/#{PACKAGING_COMMIT}/packaging/macos/com.tailrocks.velnor.runner.plist"
    sha256 PLIST_SHA256
  end

  def install
    {
      "velnorctl"       => "crates/velnorctl",
      "velnor-runner"   => "crates/velnor-runner",
      "velnor-workflow" => "crates/velnor-workflow",
    }.each do |binary, path|
      system "cargo", "install", *std_cargo_args(path: path), "--bin", binary
    end

    gh_bin = formula_opt_bin("gh") / "gh"
    odie "Homebrew gh CLI is missing: #{gh_bin}" unless gh_bin.executable?
    gh_version = Utils.safe_popen_read(gh_bin.to_s, "version").lines.first.to_s.strip
    odie "Homebrew gh CLI did not report a version" if gh_version.empty?
    env_dir = etc / "velnor"
    env_dir.mkpath
    env_file = env_dir / "velnor.env"
    unless env_file.exist?
      env_file.write <<~ENVFILE
        # Velnor macOS runner environment. Edit, then restart safely:
        #   brew services stop velnorctl
        #   $EDITOR #{env_file}
        #   brew services start velnorctl
        # Mode switching is validated at process start. Invalid or incomplete
        # modes refuse startup; the package never falls back to native-only.
        # Mac qualification order: scale-set-only, native-only, then both.
        # Keep this file mode 0600: it contains GITHUB_TOKEN.
        GITHUB_TOKEN=
        GH_TOKEN=
        # macOS host GitHub REST traffic uses system curl; native TLS can be
        # blocked by host outbound filters while curl is already allowed.
        VELNOR_GITHUB_HTTP_TRANSPORT=curl
        VELNOR_URL=https://github.com/tailrocks/velnor
        VELNOR_NAME=velnor-macos
        VELNOR_LABELS=velnor,velnor-target-mvp
        VELNOR_SLOTS=2
        VELNOR_MAX_JOBS=
        VELNOR_DOCKER_IMAGE=velnor/job-ubuntu:26.04
        # Optional Docker selection. The launcher resolves this context/host,
        # verifies the exact local Unix socket, then exports it per-process.
        # It never runs docker context use or mutates Docker configuration.
        # VELNOR_DOCKER_CONTEXT=
        # VELNOR_DOCKER_HOST=
        # Required explicit choice: native-only, scale-set-only, or both.
        VELNOR_HOST_MODE=
        # Required for scale-set-only/both. The TOML must contain:
        #   ledger_path = "#{var}/velnor/permit-ledger.db"
        #   [admission] with owner, repository, ref, source, workflow, event
        #   exactly one [auth.app] or [auth.pat] credential reference.
        # VELNOR_SCALE_SET_CONFIG=
      ENVFILE
    end
    env_file.chmod 0600

    mode_line = env_file.read.lines.find { |line| line.start_with?("VELNOR_HOST_MODE=") }
    requested_host_mode = mode_line&.sub("VELNOR_HOST_MODE=", "")&.strip
    requested_host_mode = nil if requested_host_mode.blank?
    if requested_host_mode && HOST_MODES.exclude?(requested_host_mode)
      odie "VELNOR_HOST_MODE must be native-only, scale-set-only, or both: #{requested_host_mode}"
    end

    if requested_host_mode.nil? || SCALE_HOST_MODES.include?(requested_host_mode)
      gh_help = Utils.safe_popen_read(gh_bin.to_s, "attestation", "verify", "--help")
      missing_gh_flags = GH_CLI_REQUIRED_FLAGS.reject { |flag| gh_help.include?(flag) }
      unless missing_gh_flags.empty?
        odie "Homebrew gh CLI lacks the worker attestation flags: #{missing_gh_flags.join(", ")}"
      end
    end

    storage_root = var / "velnor"
    work_dir = storage_root / "work"
    log_dir = var / "log" / "velnor"
    permit_ledger = storage_root / "permit-ledger.db"
    state_db = storage_root / "state.db"
    mode_state = storage_root / "mode-state.json"
    [storage_root, work_dir, log_dir].each(&:mkpath)
    storage_root.chmod 0700
    work_dir.chmod 0700
    log_dir.chmod 0700

    libexec.mkdir
    resource("velnor-runner-launch").stage do
      libexec.install "velnor-runner-launch"
    end
    (libexec / "velnor-runner-launch").chmod 0755

    launchd_dir = share / "velnor" / "launchd"
    launchd_dir.mkpath
    resource("velnor-launchd-plist").stage do
      replacements = {
        "__VELNOR_PREFIX__"        => opt_prefix.to_s,
        "__VELNOR_CONFIG_DIR__"    => env_dir.to_s,
        "__VELNOR_PATH__"          => "#{HOMEBREW_PREFIX}/bin:/usr/bin:/bin",
        "__VELNOR_ENV_FILE__"      => env_file.to_s,
        "__VELNOR_LOG_DIR__"       => log_dir.to_s,
        "__VELNOR_MODE_STATE__"    => mode_state.to_s,
        "__VELNOR_PERMIT_LEDGER__" => permit_ledger.to_s,
        "__VELNOR_STATE_DB__"      => state_db.to_s,
        "__VELNOR_STORAGE_ROOT__"  => storage_root.to_s,
        "__VELNOR_WORK_DIR__"      => work_dir.to_s,
      }
      replacements.each { |from, to| inreplace "com.tailrocks.velnor.runner.plist", from, to }
      launchd_dir.install "com.tailrocks.velnor.runner.plist"
    end

    metadata_dir = share / "velnor"
    metadata_dir.mkpath
    binary_digests = %w[velnorctl velnor-runner velnor-workflow].to_h do |binary|
      [binary, Digest::SHA256.file(bin / binary).hexdigest]
    end
    release_id = "macos-source-#{SOURCE_COMMIT[0, 12]}"
    runtime_dependencies = {
      "gh" => {
        "formula" => "gh",
        "binary"  => "gh",
        "path"    => gh_bin.to_s,
        "version" => gh_version,
      },
    }
    verifier_availability =
      if requested_host_mode.nil?
        "deferred-until-scale-mode-launch"
      elsif SCALE_HOST_MODES.include?(requested_host_mode)
        "verified-at-install-and-launch"
      else
        "not-required-for-native-only;-verified-if-switched"
      end
    worker_verifier = {
      "provider"                => "github-cli",
      "binary"                  => "gh",
      "preflight"               => GH_CLI_PREFLIGHT,
      "required_flags"          => GH_CLI_REQUIRED_FLAGS,
      "required_for_host_modes" => %w[scale-set-only both],
      "availability"            => verifier_availability,
    }
    host_mode = {
      "allowed"             => HOST_MODES,
      "qualification_order" => MACOS_QUALIFICATION_ORDER,
      "requested"           => requested_host_mode,
      "effective"           => nil,
      "state"               => requested_host_mode.nil? ? "unconfigured" : "not-started",
      "source"              => env_file.to_s,
      "restart"             => "brew services stop velnorctl; edit env; brew services start velnorctl",
    }
    manifest = {
      "schema"                => "velnor.homebrew-install/v3",
      "product_id"            => "velnor",
      "channel"               => "stable",
      "version"               => version.to_s,
      "release_id"            => release_id,
      "source_commit"         => SOURCE_COMMIT,
      "source_archive_sha256" => SOURCE_ARCHIVE_SHA256,
      "packaging_commit"      => PACKAGING_COMMIT,
      "host_mode"             => host_mode,
      "mode_state_path"       => mode_state.to_s,
      "runtime_dependencies"  => runtime_dependencies,
      "worker_verifier"       => worker_verifier,
      "binaries"              => binary_digests,
      "service_label"         => "com.tailrocks.velnor.runner",
      "paths"                 => {
        "config_dir"    => env_dir.to_s,
        "env_file"      => env_file.to_s,
        "storage_root"  => storage_root.to_s,
        "state_db"      => state_db.to_s,
        "permit_ledger" => permit_ledger.to_s,
        "work_dir"      => work_dir.to_s,
        "log_dir"       => log_dir.to_s,
      },
    }
    manifest_path = metadata_dir / "manifest.json"
    manifest_path.write(JSON.pretty_generate(manifest) + "\n")
    identity = {
      "schema"               => "velnor.homebrew-install-identity/v2",
      "product_id"           => "velnor",
      "version"              => version.to_s,
      "release_id"           => release_id,
      "source_commit"        => SOURCE_COMMIT,
      "runtime_dependencies" => runtime_dependencies,
      "host_mode"            => host_mode,
      "worker_verifier"      => worker_verifier,
      "manifest_sha256"      => Digest::SHA256.file(manifest_path).hexdigest,
      "binary_sha256"        => binary_digests,
    }
    (metadata_dir / "identity.json").write(JSON.pretty_generate(identity) + "\n")
  end

  service do
    name macos: "com.tailrocks.velnor.runner"
    run [opt_prefix / "libexec" / "velnor-runner-launch"]
    run_at_load true
    keep_alive true
    environment_variables(
      VELNOR_CAPABILITY_VALIDATION:    "strict",
      VELNOR_CONFIG_DIR:               (etc / "velnor").to_s,
      VELNOR_PATH:                     "#{HOMEBREW_PREFIX}/bin:/usr/bin:/bin",
      VELNOR_ENV_FILE:                 (etc / "velnor" / "velnor.env").to_s,
      VELNOR_LOG_DIR:                  (var / "log" / "velnor").to_s,
      VELNOR_MODE_STATE:               (var / "velnor" / "mode-state.json").to_s,
      VELNOR_PERMIT_LEDGER:            (var / "velnor" / "permit-ledger.db").to_s,
      VELNOR_STATE_DB:                 (var / "velnor" / "state.db").to_s,
      VELNOR_STORAGE_ROOT:             (var / "velnor").to_s,
      VELNOR_TRUST_SCOPE:              "untrusted",
      VELNOR_WORK_DIR:                 (var / "velnor" / "work").to_s,
      VELNOR_WORKER_VERIFIER:          "gh",
      VELNOR_WORKER_VERIFIER_CONTRACT: "attestation verify --help",
      MISE_LOCKFILE:                   "1",
      MISE_LOCKED:                     "1",
      MISE_LOCKED_VERIFY_PROVENANCE:   "1",
    )
    process_type :standard
    working_dir var / "velnor"
    log_path var / "log" / "velnor" / "runner.log"
    error_log_path var / "log" / "velnor" / "runner.err.log"
    stop_timeout 10800
  end

  test do
    manifest = JSON.parse((share / "velnor" / "manifest.json").read)
    identity = JSON.parse((share / "velnor" / "identity.json").read)
    env_file = etc / "velnor" / "velnor.env"
    plist = share / "velnor" / "launchd" / "com.tailrocks.velnor.runner.plist"

    assert_equal "velnor.homebrew-install/v3", manifest.fetch("schema")
    assert_equal "velnor.homebrew-install-identity/v2", identity.fetch("schema")
    assert_equal version.to_s, manifest.fetch("version")
    assert_equal version.to_s, identity.fetch("version")
    assert_equal SOURCE_COMMIT, manifest.fetch("source_commit")
    assert_equal SOURCE_COMMIT, identity.fetch("source_commit")
    host_mode = manifest.fetch("host_mode")
    assert_equal HOST_MODES, host_mode.fetch("allowed")
    assert_equal MACOS_QUALIFICATION_ORDER, host_mode.fetch("qualification_order")
    assert_nil host_mode.fetch("requested")
    assert_nil host_mode.fetch("effective")
    assert_equal "unconfigured", host_mode.fetch("state")
    assert_equal (var / "velnor" / "mode-state.json").to_s, manifest.fetch("mode_state_path")
    assert_equal "com.tailrocks.velnor.runner", manifest.fetch("service_label")
    gh_dependency = manifest.fetch("runtime_dependencies").fetch("gh")
    assert_equal "gh", gh_dependency.fetch("formula")
    assert_equal "gh", gh_dependency.fetch("binary")
    assert_match(/\Agh version /, gh_dependency.fetch("version"))
    verifier = manifest.fetch("worker_verifier")
    assert_equal "github-cli", verifier.fetch("provider")
    assert_equal ["gh", "attestation", "verify", "--help"], verifier.fetch("preflight")
    assert_equal "deferred-until-scale-mode-launch", verifier.fetch("availability")
    assert_equal manifest.fetch("runtime_dependencies"), identity.fetch("runtime_dependencies")
    assert_equal manifest.fetch("worker_verifier"), identity.fetch("worker_verifier")
    assert_equal Digest::SHA256.file(share / "velnor" / "manifest.json").hexdigest, identity.fetch("manifest_sha256")
    assert_path_exists env_file
    assert_path_exists plist
    assert_match "VELNOR_HOST_MODE=", env_file.read
    refute_match(%r{<key>VELNOR_HOST_MODE</key>}, plist.read)
    assert_match "VELNOR_MODE_STATE", plist.read
    assert_match "VELNOR_WORKER_VERIFIER", plist.read
    assert_match "attestation verify --help", plist.read
    refute_match(/__VELNOR_[A-Z_]+__/, plist.read)

    %w[velnorctl velnor-runner velnor-workflow].each do |binary|
      assert_path_exists bin / binary
      assert_predicate bin / binary, :executable?
      assert_equal manifest.fetch("binaries").fetch(binary), Digest::SHA256.file(bin / binary).hexdigest
    end
    assert_predicate libexec / "velnor-runner-launch", :executable?
    assert_match "--deny-self-hosted-runners", shell_output("gh attestation verify --help")
    assert_match "Velnor", shell_output("#{bin}/velnorctl --help")
    assert_match version.to_s, shell_output("#{bin}/velnor-runner --version")
    assert_match "0.1.0", shell_output("#{bin}/velnor-workflow --version")
  end
end
