[Ansible configs](../README.md) › [Ansible operator runbooks](README.md) › Debian release upgrade

# Debian release upgrade

Manual runbook for upgrading the Debian release on one dedicated server. Read this
when a server needs to move to a new Debian major release; it is an operator
procedure, not something a playbook performs end to end.

## On this page

- [Run the sources playbook](#run-the-sources-playbook)
- [Drain Docker](#drain-docker)
- [Upgrade](#upgrade)
- [Reboot and verify](#reboot-and-verify)

## Run the sources playbook

`upgrade-debian.yml` rewrites the Debian release sources (Hetzner mirrors) on the
target host.

```bash
ansible-playbook -i hosts.ini --limit <SERVER_NAME_HERE> upgrade-debian.yml
```

## Drain Docker

Login to server.

Stop all docker containers first:

```bash
docker stop $(docker ps -qa)
docker rm $(docker ps -qa)
docker rmi --force $(docker images -qa)
docker network rm $(docker network ls -q)
docker system prune --force
docker volume prune --force
docker volume rm $(docker volume ls -q)
```

```bash
ssh <SERVER_NAME_HERE>
```

```bash
tmux_new debianupgrade
```

## Upgrade

Update Package Lists:

```bash
apt update
```

Perform Minimal Upgrade:

`Restart services during package upgrades without asking?` choose `Yes`

```bash
apt upgrade --without-new-pkgs
```

Perform Full Upgrade:

Always choose: `install the package maintainer's version` (but check diffs before)

```bash
apt full-upgrade
```

Cleanup:

```bash
apt --purge autoremove
apt clean
```

Update GRUB config:

```bash
update-grub
```

## Reboot and verify

Reboot:

```bash
exit

reboot
```

Verify Upgrade:

Check release:

```bash
lsb_release -a
```

Check kernel:

```bash
uname -r
```
