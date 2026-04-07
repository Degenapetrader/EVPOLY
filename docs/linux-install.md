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
- checks the Linux-only EVPoly update channel inside the app

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
wget https://github.com/Degenapetrader/EVPOLY/releases/download/Linux-v1.0.40/EVPoly_1.0.40_amd64.deb -O EVPoly_1.0.40_amd64.deb
apt update && apt install -y ./EVPoly_1.0.40_amd64.deb
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

## Linux App Updates

EVPoly Linux updates use the Ubuntu `.deb` package.

That means:
- Linux updates come from `Linux-v...` releases
- Windows desktop releases do not affect the Linux updater
- if EVPoly shows an update banner on Linux, it is for a Linux package release
- Linux does **not** do a Windows-style in-place self-update

### Normal Update Flow Inside EVPoly

When EVPoly shows a new version banner:

1. Click `Download Latest .deb`
2. Your browser will open the newest Ubuntu package download
3. Download the file
4. Install that `.deb`
5. Open EVPoly again

Important:
- your profiles stay intact
- your bot settings stay intact
- this is an upgrade install, not a fresh setup

### If You Are On An Older Linux Version

Older Linux builds may still show the old `Update Now` button.

If that old button does nothing:

1. Ignore it
2. Download the newest `.deb` manually
3. Install the new `.deb`
4. Open EVPoly again

You only need to do that manual upgrade once to move onto the new honest Linux update flow.

### Easy Manual Update From The Linux Desktop

If you are already inside the Ubuntu desktop on the VPS:

1. Open the browser
2. Download the latest file named like `EVPoly_1.0.40_amd64.deb`
3. Open `Terminal`
4. Run:

```bash
cd ~/Downloads
sudo apt install ./EVPoly_1.0.40_amd64.deb
```

After install finishes:

1. Open `EVPoly` again
2. If EVPoly offers to resume the previous bot session after the upgrade, press the resume button

### Manual Update From SSH

If you prefer to update from a Windows laptop over SSH:

```bash
cd /root
wget https://github.com/Degenapetrader/EVPOLY/releases/download/Linux-v1.0.40/EVPoly_1.0.40_amd64.deb -O EVPoly_1.0.40_amd64.deb
apt update && apt install -y ./EVPoly_1.0.40_amd64.deb
```

After that:

1. reconnect to the Ubuntu desktop over Remote Desktop
2. open `EVPoly`
3. resume the previous bot session if EVPoly offers it

## First EVPoly Run

When EVPoly opens:

1. Open `Settings`
2. Create your desktop password if this is the first launch
3. EVPoly may create a starter profile for you automatically
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
apt install -y ./EVPoly_1.0.40_amd64.deb
```

## If EVPoly Opens But The Bot Does Not Start

Try this:

1. Run onboarding again
2. Save the profile again
3. Press `Start` again
4. Open `Logs` inside EVPoly if it still does not run
