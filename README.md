# FakeDNS 1.0

FakeDNS is a small portable Windows program that sends fake DNS lookups for websites
you never visit. The fake lookups are mixed in with your real ones, so your real DNS
activity is harder to pick out. It never opens those sites - it only asks DNS servers
for their addresses.

> **Important:** the lookups are ordinary, unencrypted DNS (UDP port 53). FakeDNS adds
> noise to your DNS traffic. It does not encrypt anything and does not hide your real
> lookups from your network or your ISP.

---
<img width="1920" height="976" alt="Fake" src="https://github.com/user-attachments/assets/1e2154ad-37ca-4363-945f-b3d417b00c34" />

## Features

- Portable: one `.exe` plus two folders, nothing installed.
- Modern dark GUI with a dashboard, live chart, editors for all lists, settings and a log.
- Minimizes to the system tray and keeps working in the background.
- Only one copy can run. Starting it again just brings the running window to the front.
- Optional "Run at Windows startup".
- Three kinds of host sources, which can be combined:
  - `Hosts.txt` - simple list of hostnames or URLs.
  - `Hosts.json` - hosts with subdomains.
  - Random domains - made-up names of 20-60 random letters and digits.
- Choose which DNS servers receive the lookups: your own list, or the system / network default.
- Choose which record types are sent: A, AAAA, or both.
- Daily limit from 500 to 50,000 requests (default 5,000), with random browsing-like timing.
- Never more than 8 minutes between two requests.
- The request log is kept in memory only and is never written to disk.

---

## Folder layout

```
FakeDNS.exe
App\    default data (Hosts.txt, Hosts.json, DNS-Servers.txt)
Data\   your data (your edited lists and Settings.json)
```

- When FakeDNS needs a file it uses `Data\<file>` if it exists, otherwise `App\<file>`.
- Saving a list in the GUI writes it to `Data`. Deleting a file from `Data` returns to the default.
- If the `App` folder or its files are missing, FakeDNS recreates the defaults.
- FakeDNS writes only inside its own folder. The one exception is the Windows startup entry,
  which is created only if you turn on "Run at Windows startup".
- **Do not move the program folder** if startup is enabled (see below).

Default files:

| File | Default content |
|------|-----------------|
| `Hosts.txt` | 400 popular domains (unique list, starting with `google.com`) |
| `Hosts.json` | `google.com` with subdomains `ogads-pa.clients6.google.com`, `csp.withgoogle.com`, `play.google.com` |
| `DNS-Servers.txt` | `8.8.8.8` |

---

## Quick start

1. Run `FakeDNS.exe`.
2. Open **DNS Servers** and check the list (one IP per line).
3. Open **Settings** and choose where hostnames come from.
4. Press **Start** on the Dashboard. FakeDNS never starts sending by itself when you open it.
5. Close the window with the X button: FakeDNS hides in the tray and keeps working.
6. To quit completely use **File > Exit** or the tray icon's **Exit**.

---

## The pages

### Dashboard
- Status, **Start / Stop** button.
- Tiles: requests sent today (and today's planned total), requests since launch, number of
  hosts, number of DNS servers.
- Daily budget bar.
- Chart of requests per minute over the last hour, with the peak shown above it.
- The latest lookups.

### Hosts.txt
One entry per line. Each line can be:

```
example.com
https://www.example.com/some/page
http://example.org:8080/
```

The hostname is extracted from URLs. Lines that are not valid hostnames are ignored.
Press **Save** to apply. The number of valid hosts is shown next to the button.

### Hosts.json
A list of hosts. Each host can have subdomains. Plain strings are allowed too:

```json
[
  {
    "host": "google.com",
    "subdomains": ["ogads-pa.clients6.google.com", "csp.withgoogle.com", "play.google.com", "mail"]
  },
  "example.org"
]
```

A subdomain can be a short name (`mail` becomes `mail.google.com`) or a full hostname.
In this mode one "visit" looks up the host and then its subdomains a short moment apart.
**Save** checks that the JSON is valid before saving.

### DNS Servers
One DNS server IP address per line (IPv4 or IPv6). Anything else is ignored.
A server listed twice is used twice as often.

### Settings

| Setting | What it does |
|---------|--------------|
| Host list | `Hosts.txt`, `Hosts.json`, or no list (random domains only). |
| Also use random domains | Mixes random domains into a list. A slider sets the share (1-99%). |
| Random domain TLDs | Click the chips to choose which TLDs random domains use (at least one stays on). Default: com, net, org, info, io, xyz, online, site. |
| DNS record types | **A only** (default), **AAAA only**, or **A + AAAA** (random order, like a browser). |
| DNS servers to query | The list from `DNS-Servers.txt` (default), or the system / network default DNS servers (re-checked every 5 minutes). |
| Maximum requests per day | 500 to 50,000, default 5,000. |
| www prefix | On by default. Each visit has a chance (default 50%, adjustable 0-100%) of looking up `www.<host>` instead of `<host>`. Only plain names like `example.com` get it, never names that already are subdomains. |
| Run at Windows startup | Starts FakeDNS hidden in the tray when you sign in. It starts sending only if it was running when you last used it. |

Settings are saved automatically to `Data\Settings.json`.

### Log
Every lookup with time, DNS server, record type, result and name. Only the last 5,000
entries are kept, in memory. Colors: green = answered normally, orange = answered
"no such domain", red = error or no reply.

---

## How the sending works

- **Daily total:** each day FakeDNS picks a random target between 95% and 100% of your
  maximum, so the total is never exactly the maximum and never above it.
- **Timing:** gaps are random and heavy-tailed, with short bursts, long pauses and an occasional
  longer break. The rate is the same day and night. There are never more than 8 minutes between
  requests. The minimum daily limit is 500 so that this is always possible.
- **Per visit:** one random host (or random domain) and one random DNS server are chosen.
  The A / AAAA lookups and any subdomain lookups of that visit go to the same server.
- **Random domains** do not exist, so the replies show `rcode 3` (no such domain). That is normal.
- **Results:** a lookup is only counted as answered if the reply comes from the server you
  asked and matches the query. Otherwise the log shows an error message.

---

## Tray icon and single instance

- Closing the window (X) only hides it.
- Left-click or double-click the tray icon to open the window. Right-click for **Open GUI** and **Exit**.
- Starting `FakeDNS.exe` while it is already running just shows the existing window.

## Run at Windows startup

Turning it on adds an entry for the current user that starts `FakeDNS.exe --minimized`.
FakeDNS remembers whether it was running: after sign-in it starts sending only if you had pressed
**Start** and not **Stop**. If it was stopped (or never started), it stays stopped.
The entry stores the full path of the program, so **do not move the folder afterwards**.
If you move it, turn the option off and on again from the new location.

---

## Troubleshooting

- **Log shows "no reply (timeout)":** the DNS server is unreachable or blocked by a firewall.
  Check the IP and your connection.
- **Odd servers answer anyway:** some routers and ISPs intercept port 53 and answer for any IP.
- **Window is too big for the screen:** the window has a fixed minimum size of 1180 x 700
  so everything is visible. Lower your Windows display scaling if your screen is small.
- **Old icon shown in Explorer after a rebuild:** this is the Windows icon cache. Copy the `.exe`
  to another folder or restart Explorer.
- **Nothing is sent:** make sure the status is Running, the list has at least one valid host
  (or random domains are on) and at least one valid DNS server exists.

---

## Build instructions

Requirements (Windows):

1. **Rust** with the default MSVC toolchain - install with `rustup` from https://rustup.rs
2. **Visual Studio Build Tools** with the workload **"Desktop development with C++"**.
   This is needed by Rust itself and to embed the icon into the `.exe`.
3. Internet access for the first build (it downloads the dependencies).

Steps:

1. Open a terminal in the project folder (the one that contains `Cargo.toml`).
2. Run:

```
cargo build --release
```

3. The finished program is:

```
target\release\FakeDNS.exe
```

4. Copy only `FakeDNS.exe` next to the `App` folder (the `Data` folder is created on first run).
   The `.d` and `.pdb` files next to it are build leftovers and are not needed.

Notes:

- The first build takes several minutes. Later builds are faster.
- The program is self-contained (the C runtime is linked statically) and the default list files
  are embedded in it.
- Close FakeDNS before rebuilding, including from the tray, or the build cannot replace the `.exe`.
- To try it without building a release: `cargo run --release`.
