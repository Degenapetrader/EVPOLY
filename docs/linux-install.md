# EVPoly on Ubuntu XFCE

This is the simple Linux path for people who want to run EVPoly on a dedicated VPS.

Supported target:
- `Ubuntu 24.04 XFCE`
- `x86_64`

What this package does for you:
- installs the EVPoly desktop app
- installs the bundled EVPoly bot
- installs and configures Remote Desktop support
- prepares the VPS so you can connect from a Windows laptop with `Remote Desktop Connection`

What you do **not** need:
- Rust
- Node.js
- Cargo
- VNC
- repo cloning
- manual bot sidecar setup

## Important Warning

EVPoly Linux is packed as a dedicated bot-VPS package.

That means:
- use this VPS mainly for EVPoly
- do not treat it like your normal personal computer
- do not fill it up with random extra programs if you want the smoothest result

EVPoly installs the app, the bot, and Remote Desktop support for you. Keep the machine simple.

## What You Need Before You Start

1. A VPS running `Ubuntu XFCE`
2. The VPS IP address
3. The VPS `root` password for the first install
4. The Ubuntu desktop username and password for Remote Desktop login

Important:
- you usually use `root` for the first SSH install
- you usually use the Ubuntu desktop user for the GUI login over RDP
- on many Ubuntu XFCE images, the desktop user is often something like `desktopuser`

## Easy Install From a Windows Laptop

### Step 1. Open PowerShell on Windows

Use Windows PowerShell or Windows Terminal.

### Step 2. SSH into the VPS

Replace `YOUR_VPS_IP` with your real VPS IP:

```powershell
ssh root@YOUR_VPS_IP
```

Type the VPS password when asked.

### Step 3. Run These 3 Commands on the VPS

Copy and paste these commands exactly:

```bash
cd /root
wget https://github.com/Degenapetrader/EVPOLY/releases/download/Linux-v1.0.27/EVPoly_1.0.27_amd64.deb -O EVPoly_1.0.27_amd64.deb
apt update && apt install -y ./EVPoly_1.0.27_amd64.deb
```

That install will automatically:
- install EVPoly
- install the EVPoly bot sidecar
- install `xrdp` and `xorgxrdp`
- configure Ubuntu XFCE for Remote Desktop
- open local `3389/tcp` if `ufw` is already active

## Connect From Windows Remote Desktop

After install finishes:

1. Close the SSH window if you want
2. Open `Remote Desktop Connection` on Windows
3. Enter your VPS IP
4. Connect
5. Sign in with the Ubuntu desktop user
   - usually **not** `root`
6. Open `EVPoly` from the desktop or Applications menu

You do **not** need VNC.

## First EVPoly Run

When EVPoly opens:

1. Open `Settings`
2. Create your desktop password if this is the first launch
3. Create a profile if EVPoly asks for one
4. Run onboarding
5. Save the profile
6. Go back to `Home`
7. Press `Start`

EVPoly creates its own logs, runtime files, bot config, and generated env files automatically.

## If RDP Does Not Connect

Check these first:

1. The VPS IP is correct
2. You are using the Ubuntu desktop username and password
3. Port `3389` is reachable

If you later attach a separate Vultr Firewall:
- allow inbound `TCP 3389`

## If Install Fails

Run:

```bash
apt update
apt install -y ./EVPoly_1.0.27_amd64.deb
```

## If EVPoly Opens But The Bot Does Not Start

Try this:

1. Run onboarding again
2. Save the profile again
3. Press `Start` again
4. Open `Logs` inside EVPoly if it still does not run
