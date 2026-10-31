# ansible

Configures the hosts that run machinesdancing. All software and configuration on those hosts
comes from here.

## Hosts

| Host | Group | Hardware |
|---|---|---|
| hemlock | `vm_hosts` | HP EliteDesk 800, i5-10500T (6 cores, 12 threads), 16 GB RAM, 238 GB NVMe |

## Taking over a new host

Ansible takes over once it can log in with a key and use `sudo`. The manual steps are:

1. Install Ubuntu Server LTS with OpenSSH and an admin user. Use LVM and keep the root volume
   small (about 100 GB), leaving the rest of the volume group free.
2. From the Mac: `ssh-copy-id <admin_user>@<host>`.
3. Add the host to `inventory.yml`.

From then on, the `access` role owns SSH access: it authorizes exactly the keys listed in
`group_vars/all.yml` and turns off password login.

## Running

From this directory:

```bash
ansible-galaxy collection install -r requirements.yml   # once
ansible-playbook site.yml -K --check --diff             # dry run: show what would change
ansible-playbook site.yml -K                            # apply
ansible-playbook upgrade.yml -K                         # upgrade all packages, report if a reboot is needed
```

`site.yml` only applies configuration. It never upgrades packages; `upgrade.yml` does that, on
demand. Security updates also install automatically every day (unattended-upgrades).

`-K` prompts for the `sudo` password.

## Roles

| Role | Does |
|---|---|
| `access` | Authorizes the admin SSH keys (exclusively), turns off password and root login. |
| `base` | Hostname, timezone (UTC), base packages, chrony time sync, daily unattended security upgrades (no automatic reboot by default). |
