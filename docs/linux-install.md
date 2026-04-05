# EVPoly on Ubuntu XFCE

This is the supported Linux desktop path for EVPoly v1.

Supported target:
- `Ubuntu 24.04 XFCE`
- `x86_64`

Goal:
- buy the VPS
- install EVPoly
- connect with Windows Remote Desktop
- open the app
- onboard
- click `Start`

No Rust, Node, Cargo, or manual sidecar setup is required for a normal install.

## Install

1. Download the latest EVPoly Linux `.deb` installer from the GitHub release page.
2. In XFCE file manager, double-click the `.deb` file and install it with the normal package installer.
3. If the GUI installer does not open, run this one command in Terminal:

```bash
sudo apt install ./EVPoly_*_amd64.deb
```

4. The EVPoly package automatically:
   - installs the bundled EVPoly app and bot
   - installs `xrdp` and `xorgxrdp`
   - configures XFCE for Remote Desktop login
   - opens local TCP `3389` if a host firewall such as `ufw` is already active
5. From a Windows laptop, open `Remote Desktop Connection` and connect to your VPS IP address.
6. Sign in with the Ubuntu desktop user you created on Vultr.
7. Open `EVPoly` from the desktop application menu.

## First Run

1. Open `Settings`.
2. Run onboarding.
3. Save your profile.
4. Return to `Home`.
5. Press `Start`.

EVPoly creates its own runtime folders, bundled bot sidecar, logs, and generated env files automatically.

## Notes

- `Weekend`, strategy toggles, and all normal desktop controls work the same way as Windows.
- The packaged app already includes the EVPoly bot sidecar.
- You do not need to clone the repo to run EVPoly on the VPS.
- If you later attach a separate Vultr Firewall, also allow inbound TCP `3389` there.

## Troubleshooting

If install fails because package metadata is stale, run:

```bash
sudo apt update
sudo apt install ./EVPoly_*_amd64.deb
```

If the app opens but the bot does not start:
- rerun onboarding
- save the profile
- press `Start` again
- then check `Open Logs` inside EVPoly

If Windows Remote Desktop cannot connect:
- confirm the VPS IP is correct
- confirm the Ubuntu desktop user password is correct
- confirm local port `3389/tcp` is reachable
- if you attached a Vultr Firewall later, allow TCP `3389` there too
