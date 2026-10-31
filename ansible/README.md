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

## Environment variables

Secrets never live in this repo. Pass them through the environment for the run that needs them.

| Variable | Used by | Needed when |
|---|---|---|
| `TAILSCALE_AUTHKEY` | `tailscale` role | Only the first run on a host that is not on the tailnet yet. Without it, that run stops with an error explaining how to provide it. Later runs ignore it. |

### Joining a host to the tailnet

1. **Create an auth key** in the Tailscale admin console (Settings → Keys → Generate auth key):
   - **Not ephemeral.** Ephemeral nodes are removed when they go offline, so the host would
     vanish after a reboot.
   - **Pre-approved**, if the tailnet requires device approval.
   - **Not reusable.** It is used once.
2. **Run the playbook with the key**, reading it silently so it stays out of shell history:

   ```bash
   read -s TAILSCALE_AUTHKEY && export TAILSCALE_AUTHKEY   # paste the key, press Enter
   ansible-playbook site.yml -K
   unset TAILSCALE_AUTHKEY
   ```

3. **Disable key expiry** for the host in the admin console (Machines → the host → Disable key
   expiry). Otherwise it drops off the tailnet when its node key expires, after 180 days by
   default.

The host joins under its inventory name. Tailscale SSH is not enabled: SSH access stays with the
`access` role.

## Roles

| Role | Does |
|---|---|
| `access` | Authorizes the admin SSH keys (exclusively), turns off password and root login. |
| `base` | Hostname, timezone (UTC), base packages, chrony time sync, daily unattended security upgrades (no automatic reboot by default). |
| `tailscale` | Installs Tailscale from its official apt repository and joins the host to the tailnet under its inventory name. |
| `vm_storage` | An 80 GB logical volume formatted XFS with reflinks, mounted at `/var/lib/vms`, for VM images, per-VM disks and snapshots. Fails if reflinks are unavailable. |
