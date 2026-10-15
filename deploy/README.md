# Deployment

Two units, two users, two failure domains. The core and the renderer are
separate processes so that restarting or upgrading one does not interrupt the
other, and the unit files exist to keep that true on a machine nobody is
standing next to.

## Install

On a fresh Debian machine with nothing on it but a network. A netinst has no
`curl`, so that comes first:

```bash
sudo apt install curl -y
export OVS_TOKEN=github_pat_...
curl -fsSL -H "Authorization: Bearer $OVS_TOKEN" -H "Accept: application/vnd.github.raw" https://api.github.com/repos/testbild-media/OpenVideoServer/contents/install.sh | sudo -E sh
```

The token is needed because the repository is private: an anonymous request
sees no releases at all, which looks exactly like a release that carries no
package for this architecture. The installer says so when it finds nothing and
has no token.

That fetches the newest published package for this architecture, checks it
against the checksum published beside it, and installs it with `apt` so the
dependencies come too. It also takes the Windows client from the same release
and puts it in `/usr/share/openvideoserver/client`, which is where the server's
own download page looks — so a package built without one still ends up with
something to offer. `--no-client` leaves it out.

Everything it fetches comes from this repository's releases. There is no other
host involved.

**`sh`, not `bash`.** A pipe ignores the shebang, so on Debian this runs under
dash whatever the first line of the file says. The installer is POSIX for that
reason, and is run under dash before it ships — `set -o pipefail` and
`BASH_SOURCE` were both in it, and either would have been a syntax error the
first time anybody used the command above.

It is convenient and it is a real trust decision: you are running whatever that
URL serves, as root. Read it first if you would rather — it is the same file
either way, and the honest mitigation is the checksum rather than a comment
saying the pipe is safe.

From a checkout, which builds what it installs:

```bash
sudo ./install.sh
```

It installs the graphics driver too, and restarts the machine at the end when one
was installed — a driver is no use until the kernel has let go of `nouveau`.
Either half can be declined:

```bash
sudo ./install.sh --no-driver     # leave the graphics driver to somebody
sudo ./install.sh --no-reboot     # and the restart with it
```

From a package published somewhere, named outright:

```bash
sudo ./install.sh --url https://github.com/testbild-media/OpenVideoServer/releases/download/v0.2.0/openvideoserver_0.2.0-1_amd64.deb
```

With no checkout, no `--url` and no published release, it falls back to the
address above and a **pinned** version. That host serves the one file — no
directory listing, no `latest`, no checksum — so nothing can ask it what the
newest is, and the line in `install.sh` has to be edited when a package is
published. Publishing a `latest` file beside the packages would remove that;
publishing a `.sha256` would make the download checkable, which over `http` it
is not.

The script works out which it is: a checkout beside it means build, anything
else means fetch. On a machine with no checkout and no internet, build the
package somewhere else and carry it:

```bash
./tools/package.sh                                    # on a build machine
sudo apt install ./openvideoserver_0.2.0-1_amd64.deb    # on the server
```

Either way the package creates the `ovs`, `ovs-renderer` and `ovs-console`
accounts, puts the units in place, enables and starts all three services, and
prints the address to finish at.

**The System screen comes with it and is already running.** It draws nothing and
costs nothing until a connector is marked `SERVICE_DISPLAY` — which is a role set
from the client or from the screen itself, not a file to edit. There is no
separate step and no compositor to install: it is a browser with no window, and
the renderer puts its pixels on the socket.

**The first account is made by a person, not by the installer.** It is the one
step where a password is typed, and a script that took one would put it in a
shell history and a process list. The page at `http://<this machine>/` asks for
it; on a server with no browser to hand:

```bash
sudo -u ovs ovsd --add-account <name> <password>
```

The first account made either way is an administrator, because somebody has to
be able to make the second.

### What the package will not do

- **It will not touch `/var/lib/ovs`.** Every show, the media store and the
  accounts live there. Removing or purging the package leaves it alone, and
  deleting it is one command that stays the operator's to type.
- **It cannot pull in a Vulkan driver.** Which one a machine needs is a fact
  about the machine, not about this software: `mesa-vulkan-drivers` covers Intel
  and AMD, and an NVIDIA card brings its own with the proprietary driver, which
  is a licence decision a package must not make for an administrator.
  `install.sh` does make it — it is a person running an installer — and
  `--no-driver` declines.
- **It will not overrule a unit file in `/etc/systemd/system`.** A machine set
  up by hand before this package existed has copies there, and systemd prefers
  them — so the package's units look installed and are not. The installer says
  so when it finds any, and removing them is the operator's call.
- **It will not reboot the machine.** It does install
  `/etc/modprobe.d/ovs-nvidia.conf`, which is what an NVIDIA card needs before it
  will give up more than one output, and it rebuilds the initramfs when that file
  changes — but the reboot that makes it take effect is said out loud and left to
  somebody, because an upgrade that restarted the machine would be an upgrade
  nobody could run before a show. The file is a conffile: edit it and the edit
  survives an upgrade.

  `install.sh` *does* restart, and the difference is who asked: a person running
  an installer on a machine that has just been given a driver means to finish the
  job, while `apt upgrade` may be running unattended on the afternoon of one.
  `--no-reboot` declines it.
- **It carries no NDI runtime today, which is a decision and not a
  prohibition.** This said "and cannot — the licence does not let this project
  redistribute it", and that was wrong. The SDK licence grants, at §2a, "a
  nonexclusive royalty-free license … to distribute, only in accordance with
  the SDK Documentation requirements, object code included in the SDK solely as
  used by such Products". That is how every other product in this field ships
  with NDI working out of the box.

  What it costs is obligations rather than money, and they fall on the
  licensee — whoever accepted the SDK agreement — not on this file:

  - the product's own licence must carry the terms §3d sets out: no
    modification, reverse engineering or circumvention of the SDK, no removal
    of notices, NDI's warranty and liability disclaimers, export compliance,
    NDI's copyright notice, and the same terms passed on to anyone building on
    this;
  - NDI's trademarks may be used only to say a product is *compatible with*
    NDI, with the mark noted as theirs and nothing suggesting sponsorship (§3f);
  - the bundled runtime must be the current SDK at the time it is shipped
    (§2b), so it joins the list of things a release has to keep up to date.

  **And it would not go where it is now.** The SDK documentation is explicit:
  "You should include the NDI DLLs as part of your own application and keep
  them in your application folders … Please do not install your NDI DLLs into
  the system path for this reason" — the reason being a collision with another
  application's copy. This package already has an application folder with its
  own FFmpeg in it and an RPATH pointing there, which is exactly where the
  runtime would belong. Installing it into `/usr/local/lib`, as the section
  below does, is what the SDK asks you not to do.

  **Decided on 2026-10-15: it is carried.** `tools/package.sh --ndi=<file>`
  stages the library in the package's own library folder, under the soname
  read out of the file, and refuses to build if no licence text is found
  beside it. The release workflow fetches a `.tar.gz` of the library and its
  licence from a URL held in the `NDI_RUNTIME_URL` repository secret; without
  the secret nothing is fetched and the package carries no NDI, which is what
  a fork or a pull request builds.

  The library is deliberately **not** in this repository: it is public, and
  the SDK is confidential under 4a of the agreement. What may be distributed
  is the runtime inside the product, which is what the `.deb` is.

  A package built without the runtime has no network sources and no network
  outputs, and says so once at startup and nowhere else — which is exactly how it was met: a server that
  "sees no NDI on the network", and one INFO line in the journal that nobody
  had been told to look for.

  ```bash
  journalctl -u ovs-renderer -b | grep -i ndi
  ```

  ```text
  the NDI runtime is not installed on this machine (tried libndi.so.6,
  libndi.so, Processing.NDI.Lib.x64.dll); no network sources or outputs
  will be offered
  ```

  **Installing it.** The renderer opens `libndi.so.6` by name when something
  first asks for it, so all that is needed is that library, somewhere the loader
  looks.

  The SDK comes from NDI — <https://ndi.video/for-developers/ndi-sdk/> — and
  not from here. It is not in this repository, not in the package, and not on
  the server: the same licence that stops the package carrying the runtime
  stops it carrying the SDK. Download *NDI SDK for Linux*, put the archive on
  the machine, and:

  ```bash
  tar xzf Install_NDI_SDK_v6_Linux.tar.gz && sh ./Install_NDI_SDK_v6_Linux.sh
  ```

  It shows the licence and will not go on until you type `y`, which is a
  decision for whoever runs the machine and not for an installer — this is the
  reason none of it can be automated here. It unpacks into `NDI SDK for Linux`
  in the current directory, and the library is under `lib/`:

  ```bash
  sudo install -m 0644 \
    "$(readlink -f "NDI SDK for Linux"/lib/x86_64-linux-gnu/libndi.so.6)" /usr/local/lib/
  sudo ldconfig
  sudo systemctl restart ovs-renderer
  ```

  **One file, and `ldconfig` makes the rest.** The SDK ships `libndi.so` and
  `libndi.so.6` as symlinks beside the real `libndi.so.6.3.2`, and `install`
  follows a symlink rather than copying it — so `libndi.so.*` installed three
  *copies* of twenty-seven megabytes, and `ldconfig` then said
  `libndi.so.6 is not a symbolic link` on every package operation afterwards.
  That is what `readlink -f` is for here: it names the real file whatever its
  version, and `ldconfig` reads the `SONAME` out of it and makes
  `libndi.so.6` point at it, which is the arrangement the loader expects.

  A machine that already has the copies is tidied the same way:

  ```bash
  sudo rm /usr/local/lib/libndi.so.6 && sudo ldconfig
  ```

  Safe while the renderer is running: it holds the file it already opened, and
  picks up the link the next time it starts.

  The same `journalctl` line then names the sources it can see instead. Nothing
  else about the server changes: a machine without it loses NDI and nothing
  else, which is why this is not part of installing one.

  **A sender that discovery does not find.** Not every NDI sender announces
  itself over mDNS, and some that do are not heard across a particular switch
  or VLAN. A PTZ camera on 2026-10-10 was on the server's own segment, answered
  ping in 0.35 ms and had its sender ports open — and was invisible to
  discovery. NDI's own reference finder, built from the SDK's
  `examples/C++/NDIlib_Find` and run on that machine, found nothing either,
  which is how we knew the server was not at fault.

  Such a sender is named by address. NDI's runtime reads its own configuration
  and nothing here parses it:

  ```bash
  sudo install -d -m 0755 /etc/openvideoserver/ndi
  sudo tee /etc/openvideoserver/ndi/ndi-config.v1.json > /dev/null <<'JSON'
  { "ndi": { "networks": { "ips": "10.110.0.33,10.110.0.34", "discovery": "" } } }
  JSON
  ```

  ```bash
  # /etc/systemd/system/ovs-renderer.service.d/ndi.conf
  [Service]
  Environment=NDI_CONFIG_DIR=/etc/openvideoserver/ndi
  ```

  ```bash
  sudo systemctl daemon-reload && sudo systemctl restart ovs-renderer
  ```

  `ips` is a comma-separated list. `discovery` names an NDI Discovery Server
  instead, which is the better answer for a rig that has one: one setting rather
  than a list that goes stale the day a camera is readdressed.

  **Proving it before changing the service**, which is how the above was
  arrived at rather than guessed:

  ```bash
  cd "$HOME/NDI SDK for Linux"
  g++ -std=c++17 -I include examples/C++/NDIlib_Find/NDIlib_Find.cpp \
    -o /tmp/ndi-find -L lib/x86_64-linux-gnu -lndi \
    -Wl,-rpath,"$PWD/lib/x86_64-linux-gnu"
  NDI_CONFIG_DIR=/etc/openvideoserver/ndi timeout 12 stdbuf -oL /tmp/ndi-find
  ```

  `stdbuf -oL` is not decoration: without it the output is block buffered and
  `timeout` kills the process before any of it is flushed, so a finder that
  worked looks like one that found nothing.

### What is in the package

The three binaries, **their own FFmpeg**, **the Chromium the System screen
draws with**, the three unit files, the licences, these notes, and the Windows
client installers if any were put in `dist/client` before building.

That Chromium is why the package is around 130 MB. `libcef.so` is 1386 MB as it
is distributed, because it carries its debug information; the package strips that
and ships 443 MB of library, which xz takes to about 107 MB. Nothing else in here
is stripped — a backtrace out of a rack at midnight is worth more than the
megabytes. It also brings the thirty-two system libraries Chromium needs, asked of
`dpkg-shlibdeps` on `libcef.so` itself rather than guessed: without that a minimal
Debian installs the package and the System screen cannot load its own library.

The package refuses to build without a framework rather than leaving it out. It
installs `ovs-console.service` and creates the account that runs it, so a package
missing the framework is a machine whose System screen cannot start — which is
what it shipped as until 2026-09-26. The server offers those for download on its
status page, which is the point: a second laptop at a venue with a captive
portal can still get the client.

#### Why it brings its own FFmpeg

Debian's is GPL-2.0-or-later — its copyright file says the default packages are
built with GPL sources — and this is an Apache-2.0 product. So the package
carries the build `tools/build-ffmpeg.sh --profile server` makes, which refuses
to leave behind anything configured with `--enable-gpl`.

The libraries go in `/usr/lib/openvideoserver`, a private directory, so a
machine's own FFmpeg is untouched and nothing else on it links against these.
The binaries find them through an old-style `DT_RPATH` rather than the
`DT_RUNPATH` a linker emits by default, and that distinction is load-bearing:
`RUNPATH` is consulted only for the object that declares it, so `libavcodec`
would go looking for `libavutil` down the system path and find Debian's — half
of one build and half of another, inside a process that decodes video.
`package.sh` refuses to build a package whose binaries lack that `RPATH`, and
refuses again if the derived dependencies mention Debian's FFmpeg at all.

The change cost nothing the server does: all fifteen codecs the product asks
about are in both builds, hardware decode and encode are unchanged, and the
software preview encoder moves from `libx264` to `libopenh264` and gets two and
a half times cheaper. ProRes decode in software is 2.2% dearer. The numbers are
in the header of `tools/build-ffmpeg.sh`.

The dependencies are read out of the binaries with `dpkg-shlibdeps` rather than
written down, so a package built on this Debian release names this release's
libraries. `libvulkan1` is the one exception and is declared by hand: the
renderer opens Vulkan at runtime rather than linking it, so nothing can derive
it — and without it the package installs cleanly onto a machine where the wall
then stays black.

## Installing by hand

What the package does, for a machine where it is not wanted:

```bash
sudo apt install seatd
sudo systemctl enable --now seatd

sudo useradd --system --no-create-home --shell /usr/sbin/nologin ovs
sudo useradd --system --no-create-home --shell /usr/sbin/nologin ovs-renderer
sudo useradd --system --no-create-home --shell /usr/sbin/nologin ovs-console

sudo install -m 0755 target/release/ovsd /usr/bin/ovsd
sudo install -m 0755 target/release/ovs-renderer /usr/bin/ovs-renderer
sudo install -m 0644 deploy/systemd/*.service /etc/systemd/system/

sudo systemctl daemon-reload
sudo systemctl enable --now ovs-renderer ovsd

# The touchscreen, if this machine has one (see "The System screen" below).
# Nothing to install: the browser it draws with is installed by `deploy.sh`
# alongside the binaries, and it opens no device.
sudo systemctl enable --now ovs-console
```

`ovsd` creates `/var/lib/ovs` through systemd's `StateDirectory`, so a fresh
install needs no manual setup. The database and the media store both live there:
a backup of that directory is a backup of the server.

## Updating a machine that is already running one

```bash
./tools/deploy.sh --check    # build, and say what differs from what is installed
./tools/deploy.sh            # install it, restart, and check the wall came back
```

Run from the checkout on the server itself. It keeps what it replaced in
`/var/lib/ovs/previous`, restarts both units, and waits for an output to report
a frame rate again — and if none does within forty seconds it puts the old
binaries back, restarts, and prints the renderer's last twenty log lines. A
renderer that cannot open the card leaves a dark wall and no error anybody is
looking at, which is the failure this is for.

It deliberately builds rather than copying a binary from elsewhere: one built
against this machine's FFmpeg and this machine's driver is the only one worth
installing on it.

## Order

There is none, on purpose. The renderer listens and the core connects to it, so
either may start first and either may be restarted alone. `ovsd` retries until
the renderer answers and reports `unreachable` in the meantime, which is a
different thing from `stopped` — the renderer may be rendering perfectly well
and simply not be reachable.

## Checking it

```bash
systemctl status ovsd ovs-renderer
journalctl -fu ovs-renderer          # one line per output per second
curl -s localhost:7420/api/v1/server/capabilities | jq
```

The System screen shows the same thing with the outputs' actual pictures beside
their frame rates — on the machine itself, at `http://127.0.0.1:7420/console`.

## Being found

A client lists the servers on its network rather than asking for an address, so
a server carried to another venue is clicked rather than looked up. That works
on its own — there is nothing to configure — and the machine's name in the list
is `name` in `/etc/openvideoserver/config.json`, falling back to the host name.

To check it from the machine itself:

```bash
avahi-browse -tr _openvideoserver._tcp
```

**If the list is empty, the unit file is the first thing to look at.** The
responder asks the kernel which interfaces exist, and on Linux that question is
a netlink socket: a unit whose `RestrictAddressFamilies` leaves out `AF_NETLINK`
gets a responder that registers, reports that it is announcing, and announces on
nothing at all. That was true of this repository's own unit until 2026-09-24 and
is exactly the shape of failure to lose a day to — the server said one thing and
the network saw another.

The server checks itself now. A few seconds after it starts it listens for its
own announcement and writes one line either way:

```bash
journalctl -u ovsd | grep -E "found on the network|never heard it"
```

A network that filters multicast looks identical from the machine's side and
needs nothing done to the server: clients on it type the address, which is what
they did before.

## Configuring the machine

One file, edited once, when the rack is built:

```json
// /etc/openvideoserver/config.json
{
  "service_display": {
    "gpu_pci_address": "0000:00:02.0",
    "connector": "monitor",
    "drm_connector": "eDP-1"
  }
}
```

It says one thing: **which connector carries the System screen**. That is the
setting which has to be right before anything else can be — the renderer must not
take that socket even for its first frame — and it is not in the database on
purpose: an operator commissioning a rack edits a file, recovering a machine
means copying one, and a server whose screen is black because a row is missing is
one nobody can fix from the screen.

The first two fields are what **Vulkan** calls the socket; run
`ovs-renderer --displays` to read them off the machine. `drm_connector` is what
the **kernel** calls the same socket, from `/sys/class/drm`: the two layers
genuinely disagree — Mesa calls a panel `monitor` where the kernel calls it
`eDP-1` — and it is what lets the server say at startup whether that panel is
plugged in at all. A fourth field, `drm_device`, overrides the card node and is
almost never needed; the PCI address finds it.

### From the administration page instead

The page at `http://<this machine>/admin` sets the same two things — the name
and the System screen's connector — and offers the connectors the renderer
found, so nothing has to be typed from a second terminal. It writes this file.

Three things it is careful about, because each has already cost somebody a
drive or could:

- **It takes effect at the next start, and says so.** The connector is reserved
  when the renderer starts and the kiosk is pointed at a card before its
  compositor opens one. A Save button that implied a screen had moved would be
  lying.
- **It says when the connector is not one the renderer reports.** Normal on a
  machine whose renderer is not running; a typo otherwise.
- **It says when the client disagrees.** `SERVICE_DISPLAY` can also be assigned
  to a connector in the client, and the two can name different sockets. This
  file wins, because it is the only one of the two that can be read before the
  renderer starts — so the disagreement is reported rather than quietly
  resolved.

The file stays the answer for a machine being commissioned before it has an
account, and for one being recovered from a copy.

The card node is found rather than written down because `/dev/dri/card0` is not
stable: which card is zero depends on which driver bound first, and installing a
graphics driver is enough to swap them. The PCI address does not move.

The System screen's own unit turns this into the two lines its compositor needs,
immediately before starting it:

```ini
ExecStartPre=/usr/bin/ovsd --write-kiosk-env /run/ovs-console/console.env
EnvironmentFile=-/run/ovs-console/console.env
```

which is a change from `ovsd` writing that file at its own pace. On a two-card
machine those two started in the same second, the compositor read the file one
second before it was written, took every card it could find, and put the System
screen on whichever panel happened to work — the external one. Generating it
inside the start that consumes it removes the window entirely.

What it pins is the **card**, not the connector: wlroots has no variable for
choosing a connector (`WLR_OUTPUTS`, which an earlier build wrote, is not one and
never did anything), and a card carries one master anyway — see *One GPU cannot
do both* below.

Everything about a *show* — which walls exist, what they run at, what is mapped
onto them — is in the show, not here. A server is bought once and re-purposed
all evening.

## The System screen

It is a touchscreen in the rack, so it is **served only to the machine it runs
on and asks for no password**. From anywhere else `/console` does not exist, and
says so with a 404 rather than a login nobody could complete.

The same rule covers what the page then asks for: a request that arrives over
the loopback interface is treated as coming from the machine itself and needs no
session, and every request that arrives over a network still does. That is a
real decision and worth stating plainly — **anything running on this server can
read the show's state and stop the machine without a password**. On an appliance
in a rack that is the point; somebody who already has a shell on the box can do
worse than press Shutdown. A deployment that disagrees turns it off:

```ini
# /etc/systemd/system/ovsd.service.d/auth.conf
[Service]
Environment=OVS_TRUST_LOOPBACK=0
```

with the consequence that the touchscreen then shows "this server requires a
session; the local screen is switched off", which is the honest outcome.

Nothing is taken from `X-Forwarded-For` or `Forwarded`, and the header the
server marks local requests with is stripped from every incoming request before
it is looked at — so a client cannot claim to be local. But a reverse proxy on
this machine *is* local as far as the kernel is concerned: do not put one in
front of `ovsd` and then expose it.

`ovs-console.service` runs `/usr/bin/ovs-console`, which hosts a browser with
**no window**: it lays the page out, paints it into memory, and hands the pixels
to the renderer over a Unix socket. The renderer puts them on one connector like
any other output.

It opens no device at all. No seat, no `seatd`, no Wayland socket, no `video`
group — all of which the kiosk compositor before it needed, and all of which were
found by deploying it rather than by reading a manual.

Two things need to be true:

- **the connector's role must be `SERVICE_DISPLAY`**, which is what makes the
  renderer drive it with the console instead of offering it to the show. Set it
  in the admin screen, beside the mode to drive it at.
- **the renderer must be in the console's group**, because the socket is 0660 and
  connecting to a Unix socket needs *write* permission. The package does this;
  a hand install needs `adduser ovs-renderer ovs-console`.

Chromium's framework lives in `/usr/lib/openvideoserver/cef` — beside the
product rather than in `/usr/lib`, so nothing else on the machine can pick it up
by accident. It is 1.5 GB, and `deploy.sh` compares before copying.

## Asking the machine whether it works

Four tools, all run on the server itself, because the API answers a request from
the machine with no session and a measurement taken across the
network measures the network.

```bash
./tools/acceptance.sh all        # the things an operator moves reach the wall
./tools/reference-load.sh 60     # three clips at once, the specified workload
./tools/soak.sh 15 night.csv     # a row every fifteen seconds, until stopped
python3 tools/stream-latency.py  # how long a change takes to reach a watcher
```

**`acceptance.sh`** changes something through the API an operator would use and
reads the composition back as a picture. Every feature in this product has unit
tests and every one of them passes on a machine with no display attached; what
they cannot tell you is whether the thing an operator moves reaches the wall.
It covers an effect, every blend mode, the crossfader and dimmer, freeze, the
master clock's grid, and — with `OVS_SEND` naming an `ovs-dmx-send` binary — a
lighting console. `acceptance.sh recover` is separate because it kills the
renderer to time how long the wall is dark.

**`reference-load.sh`** plays one clip on one, two or three layers and watches
the frame rate, the decoder count and the late count. `tools/make-reference-clip.sh`
makes a clip to measure it with — grainy on purpose, because an all-intra codec
spends its bits on detail and a synthetic pattern encodes three times lighter
than the format really is.

**`soak.sh`** writes a row every so often and nothing else. `--summary <file>`
reads it back: what the rate did against the mode it was asked for, whether
either process grew, how hot it got, and whether either of them restarted. The
failures that end an evening are the ones a test suite cannot find.

**`stream-latency.py`** speaks WebSocket itself and times a change from the API
call to the picture carrying it arriving. It needs nothing installed.

None of them changes anything permanently: each puts the show back as it found
it and leaves the machine showing the show.

## Proving the lighting network

The server receives Art-Net and sACN and never transmits, so there is nothing
in it that can test the receiving side. `ovs-dmx-send` is a separate program for
that, run from another machine on the same network:

```bash
cargo run -p ovs-dmx --bin ovs-dmx-send -- \
  --host 192.0.2.10 --protocol artnet --universe 0 --pattern ramp --seconds 5

cargo run -p ovs-dmx --bin ovs-dmx-send -- \
  --host 192.0.2.10 --protocol sacn --universe 1 --priority 120 \
  --source "desk A" --channels 1=255,2=128 --seconds 5
```

`GET /api/v1/server/status` then shows the universe under `telemetry.dmx`, with
the source that is driving it and the priority it claimed. Running two of them
at once on one universe with different priorities shows the arbitration: the
higher one takes it, and the lower one takes it back about two and a half
seconds after the higher stops, which is E1.31's own data-loss timeout.

Nothing in `ovsd` or the renderer links this tool and it cannot be reached
through the API. It is not the server learning to transmit.

## A machine with an NVIDIA card

**`install.sh` does this for you**, and `--no-driver` is there for somebody who
would rather do it themselves. By hand it is Debian's own driver with the kernel
headers that build it:

```bash
# contrib, non-free AND non-free-firmware first — all three
sudo apt install linux-headers-amd64 nvidia-driver firmware-misc-nonfree
```

**All three, and this file said two.** `nvidia-driver` and `nvidia-kernel-dkms`
are in `non-free`; `nvidia-support` and `glx-alternative-nvidia` are in
`contrib`; only `firmware-misc-nonfree` is in `non-free-firmware`. With the two
this used to name, apt answers "Package 'nvidia-driver' has no installation
candidate". Asked of Debian's archive on 2026-09-26 rather than remembered.

Two things then need saying out loud.

**Secure Boot.** A module DKMS builds is not signed by anything the firmware
trusts, so it refuses to load — `Key was rejected by service`, with
`Loading of module with unavailable key is rejected` in `dmesg`. The fix is to
enrol DKMS's own key once, which needs somebody at the machine:

```bash
sudo mokutil --import /var/lib/dkms/mok.pub   # choose a one-time password
sudo reboot                                   # then: Enroll MOK, Continue, the password
```

Afterwards `/etc/dkms/framework.conf` should name that key
(`mok_signing_key`, `mok_certificate`, `sign_tool`) so every future kernel signs
itself. A module recompressed by hand must keep the kernel's own xz settings —
`xz --check=crc32 --lzma2=dict=1MiB` — or it loads as far as
`decompression failed with status 6`.

**Its KMS driver has to be out of the way.** `options nvidia-current-drm
modeset=0` in `/etc/modprobe.d`, or the card gives up exactly one output however
many sockets it has. Installing this server puts the file there and rebuilds the
initramfs; what it will not do is reboot, and until something does, the running
kernel keeps what it booted with. See "An NVIDIA card needs its KMS driver out of
the way" below for what it does and which module name actually works.

**The renderer needs more than /dev/dri.** An NVIDIA card is reached through
`/dev/nvidiactl` and `/dev/nvidia0` as well, and `ovs-renderer.service` names
those device classes. Without them the driver fails quietly, Vulkan offers only
the other card, and a machine with a discrete GPU in it renders the show on the
processor's own graphics — running, composing, reporting system memory as "video
memory" and a dash where the GPU load should be. The unit ships with them; this
is here because the symptom looks like anything but a permissions problem.

## The card's bus, which can quietly run at an eighth of its speed

**Check this on every machine before believing any throughput number from it.**

```bash
nvidia-smi --query-gpu=pcie.link.gen.current,pcie.link.gen.max --format=csv
cat /sys/bus/pci/devices/0000:01:00.0/current_link_speed   # the card's own address
```

A laptop card was found negotiated at **2.5 GT/s — PCIe 1.0 — against the 16 GT/s
it supports**, eight lanes wide either way, while sitting at performance state P0
drawing 24 watts on mains power. Not idle downclocking, and not a missing driver:
the proprietary module was bound and current, the card owned the panel, and
`nvidia-smi` reported the host port capable of PCIe 5.0 and the card capable of
PCIe 4.0. Both ends could; both were parked.

What it cost is the whole of a show. Getting one 1080p frame onto the card is a
copy, and that copy sits inside the command buffer the composition submits and
the render thread waits for:

| | on PCIe 1.0 | on PCIe 4.0 |
|---|---|---|
| one layer, composing | 12 972 us | 1 323 us |
| three layers, composing | 30 796 us | 3 135 us |
| three layers, the whole pass | 197% of a frame at sixty | 29% |

A frame at sixty is 16 667 microseconds. On the slow link three 1080p60 clips
cannot fit in one and the wall runs at forty frames a second; on the fast one
they use under a third of it. Same machine, same card, same clips, same build.

**What brought it up** was turning the kernel's PCIe power management to
performance and then re-initialising the card by restarting the renderer. After
that the driver scales the link with demand the way it is supposed to — 16 GT/s
at P0, 8 GT/s at P4 with three clips playing, and the wall holds sixty at either,
because three layers cost about nine milliseconds of a frame even at the halved
rate. What was wrong before was not that it scaled; it was that it sat at 2.5
GT/s while the card was at P0 under sustained load.

```bash
echo performance | sudo tee /sys/module/pcie_aspm/parameters/policy
sudo systemctl restart ovs-renderer
```

**That does not survive a reboot**, because the policy resets to `default`. Make
it stick on the kernel command line — the same file the outputs section already
edits:

```bash
# /etc/default/grub: add to GRUB_CMDLINE_LINUX_DEFAULT, then update-grub
pcie_aspm.policy=performance
```

Turning off the card's own runtime power management as well does no harm and is
one line, though on its own it changed nothing here:

```bash
# /etc/udev/rules.d/60-ovs-gpu-power.rules
ACTION=="add", SUBSYSTEM=="pci", ATTR{vendor}=="0x10de", ATTR{class}=="0x030000", ATTR{power/control}="on"
```

If the link still will not train, look in the firmware setup for a PCIe link
speed or "PCIe Gen" setting, and for a graphics mode that hands the display to
the discrete card outright rather than through the integrated one. And check
whether there are retimers in the path — `lspci -vv` says `Retimer+` — because a
link through two of them sometimes does not train higher whatever is configured.

`ovs-upload-cost` prints the link speed beside its figures for exactly this
reason: a slow copy on a slow bus and a slow copy on a fast one look identical
and are not the same finding.

## One GPU can do both, and this is how

**A renderer using the `display` backend takes DRM master on the whole card, not
on one connector.** Enumerating the connectors is enough to do it. That is still
true, and it used to mean a card carrying a show could not also carry the Service
Display: the kiosk compositor opened the card, found it already had a master, and
failed every swapchain test it tried. The screen stayed black.

The answer was not DRM leasing. It was to stop having two things that want a
card. The System screen is drawn **off-screen** now — a browser with no window,
painting into memory — and the renderer, which already holds the card, puts those
pixels on one connector exactly as it puts a wall on another. One master, one
card, every socket on it available for either job.

So a single-GPU machine, which the reference server is, needs no second adapter
and no arrangement at all: mark a connector `SERVICE_DISPLAY` and it carries the
console; mark it `UNUSED` and the show may have it. The owner's rule is the one
the code now keeps — "Das System Display KANN und DARF KEINE ganze Karte nehmen.
Es darf sich nur den Ausgang nehmen".

### An NVIDIA card needs its KMS driver out of the way

**One line, and without it a card carries one output.** Both
`tools/deploy.sh` and the package install it, rebuild the initramfs when it
changes, and then tell you a reboot is owed:

```conf
# /etc/modprobe.d/ovs-nvidia.conf, from deploy/modprobe.d/ — then: sudo reboot
options nvidia-current-drm modeset=0
options nvidia-current NVreg_PreserveVideoMemoryAllocations=1
```

With `modeset=1`, which is what the driver ships enabled, the NVIDIA KMS driver
owns the card's connectors. `VK_KHR_display` then cannot take the one it is asked
for: `vkAcquireDrmDisplayEXT` refuses even with the process holding DRM master on
the card. One output comes up, a second one does not, and nothing says why —
`ovs-planes` was run before and after a reboot on 2026-09-26 to establish exactly
that, and it is the difference between a card driving a wall and a card driving a
wall *and* the System screen.

**The module name is the trap.** Debian's packaging calls it
`nvidia-current-drm`, so the `nvidia-drm.modeset=0` that every forum post
recommends on the kernel command line reaches nothing at all, silently. Check
what actually took effect rather than what was asked for:

```bash
ls /sys/class/drm/            # with modeset=0 the NVIDIA card registers no
                              # KMS device and contributes no connectors here
```

The second line is unrelated to any of this and is here because it belongs in the
same file: it tells the driver to keep video memory across suspend, without which
a machine that sleeps wakes up with a card full of nothing.

`OVS_RENDERER_BACKEND=vulkan` remains the fallback for a machine with nothing
plugged in — it composes and presents nowhere. The renderer says which backend it
is using at startup either way.

## When the System screen's panel will not take a mode

Seen once, on the test machine, and worth recognising: the compositor comes up,
says `Swapchain for output 'eDP-1' failed test` ten times a second and shows
nothing, while the kernel console is still perfectly visible on that same panel.
Every renderer fails the same way, so it does not look like a graphics problem —
and it is not one. `modetest -M i915 -s <connector>:#0` fails too, with
`Invalid argument`, which is the useful thing to try: it takes the compositor out
of the question entirely.

With `echo 0x16 > /sys/module/drm/parameters/debug` the driver says why. On that
machine it was computing a link rate of 216000 for a panel already running at
540000 — half the link, not enough for any mode the panel offers, so every mode
was refused while the one set at boot carried on working. A reboot cleared it and
it has not come back.

The lesson for an operator is the order to work in: the compositor's log says
what failed, `modetest` says whether anything can set a mode at all, and the DRM
debug flag says what the driver thinks the panel can carry. The lesson for this
show is that the System screen failing is a screen, not a show: the outputs
were driving throughout.

## Letting the screen reboot the machine

The package depends on `polkitd`, and that is not decoration: without it nothing
reads the rule below, and `systemd-logind` refuses a caller that has no session
outright. The System screen's two buttons then answer `Call to Reboot failed:
Access denied` — which is a different sentence from `Interactive authentication
required`, and the difference is the diagnosis. The first means no polkit; the
second means a rule that is there and does not match.

The System screen offers Restart and Shut down, each behind a six-second hold
. Both run `systemctl reboot` and `systemctl poweroff` as the
`ovs` user,
which is refused unless the machine has been told to allow it. Without this the
buttons are still there and still say what happened — "refused it; check the
unit's permissions" — rather than doing nothing:

```ini
# /etc/polkit-1/rules.d/50-ovs-power.rules
polkit.addRule(function(action, subject) {
  if (subject.user == "ovs" &&
      (action.id == "org.freedesktop.login1.reboot" ||
       action.id == "org.freedesktop.login1.power-off")) {
    return polkit.Result.YES;
  }
});
```

A server that nobody should be able to reboot from the screen is a server that
simply does not get this file. That is the decision it is meant to be.

The renderer can also be restarted on its own, which needs no privileges at all:
the core asks it to stop and systemd's `Restart=always` starts it again, the
same path a crashed renderer takes. That action is on the desktop client rather
than on the System screen, which the owner's drawing gives three buttons.

## Outputs that are not plugged in yet

Outputs are forced onto connectors: a show names `DP-3` and the
renderer drives `DP-3`. What it cannot do is drive a socket the *kernel* thinks
is empty, and an LED processor that is powered from the same distro as the wall
is exactly that for the first minutes after a boot — as is one that asserts no
hotplug signal at all, which many do.

Tell the driver the socket is occupied, with the mode it runs at:

```ini
# /etc/default/grub, then update-grub
GRUB_CMDLINE_LINUX_DEFAULT="video=DP-3:1920x1080@60e video=DP-4:1920x1080@60e"
```

The `e` is "enabled": force the connector on whether or not anything answers.
`d` forces one off, which is how a connector nothing should ever drive is taken
out of the picture entirely.

For a machine already running, the same thing without a reboot:

```bash
echo on | sudo tee /sys/kernel/debug/dri/0/DP-3/force
```

## A mode a connector does not advertise

**On the NVIDIA proprietary driver there is no way to give a connector a mode it
did not derive itself.** Not a preference and not a missing feature here: five
routes, all tried on the reference machine on 2026-09-25 (RTX 4070 Laptop,
NVIDIA 550.163.01, kernel 6.12, a capture card presenting no EDID), all refused.

| Route | What happened |
|---|---|
| `vkCreateDisplayModeKHR` for an unlisted mode | `ERROR_INITIALIZATION_FAILED`. Every *listed* mode built, so the call works and the refusal is about the mode. |
| `echo on > /sys/kernel/debug/dri/1/HDMI-A-5/force` | `Operation not permitted`, as root. |
| `echo <edid> > .../HDMI-A-5/edid_override` | `Operation not permitted`, as root. |
| `drmModeSetCrtc` with a hand-built 1080p60 modeline | `EINVAL`. KMS itself refuses it, so this is not about the presentation path. |
| `video=HDMI-A-5:1920x1080@60e` on the kernel command line, then a reboot | No effect. The parameter is on `/proc/cmdline` and the connector's mode list is unchanged. |

The last one is the surprise and the reason this table exists: `video=` is read by
`drm_kms_helper`, and NVIDIA's driver builds its mode list without going through
that helper. The kernel parses the parameter and the driver never asks.

`ovs-modes` prints both halves on any machine — what each connector offers, and
what the driver will build when asked. Run it before promising an operator a
resolution:

```bash
sudo systemctl stop ovs-renderer
ovs-modes 1920x1080@60
sudo systemctl start ovs-renderer
```

So a connector with no EDID runs what the driver invented for it, which is a
handful of VGA timings. What does work, in the order worth trying:

- **Give it an EDID.** An HDMI EDID emulator between the card and whatever is
  downstream makes the connector advertise the mode, and everything above works
  unchanged. This is the ordinary fix in a rack and it costs a few euros.
- **A processor that presents its EDID.** Most LED processors do; a capture card
  and a matrix in the way often do not.
- **A different GPU.** Intel and AMD honour `video=` and take user modelines
  through KMS, which is what Weston relies on to force a mode with no EDID.

This is worth knowing before a commissioning day, which is why it is written
down rather than rediscovered.

`ovs-renderer --displays` then lists the connector and its modes. A connector
that does not appear there is one no show can be mapped onto, and on a
commissioning day the reason is a cable, a powered-off processor, or this file.

## Lighting network

`ovsd` listens for Art-Net on UDP 6454 and sACN on UDP 5568, both with address
reuse so another tool on the same machine is not shut out. Nothing is sent: the
server does not answer ArtPoll and does not appear in a console's node list.

```bash
sudo ufw allow 6454/udp comment 'Art-Net'
sudo ufw allow 5568/udp comment 'sACN'
```

On a machine with a separate lighting network, say which interface sACN's
multicast joins go out of — picking the wrong one is the usual reason a receiver
sits there hearing nothing at all:

```ini
# /etc/systemd/system/ovsd.service.d/dmx.conf
[Service]
Environment=OVS_DMX_INTERFACE=10.0.1.20
```

`OVS_DMX=off` opens neither socket, for a server that shares a machine with
something else that owns the lighting ports.

To programme a console, use the channel lists in [`reference/FIXTURES.md`](../reference/FIXTURES.md),
or ask the running server for the version it actually has:

```bash
curl -s -H "authorization: Bearer $TOKEN" localhost:7420/api/v1/fixtures | jq
```

## Which FFmpeg the server links

By default it links the machine's: on Debian that is `libavcodec-dev` and
friends, which is a build configured `--enable-gpl`. That is fine for a server
you built and run yourself and it is not fine for one you hand to somebody,
because linking is not the arm's-length relationship running a program is
(owner decision Q18).

`tools/build-ffmpeg.sh --profile server` builds the alternative: the same
FFmpeg release, configured without any GPL component, with `libopenh264` in
place of `libx264` for the software preview encoder, and with the NVIDIA decode
and encode paths the renderer needs. It installs into `build/ffmpeg-server`.

```bash
./tools/build-ffmpeg.sh --profile server
```

To build the server against it, point `pkg-config` at it and put its libraries
on the loader's path:

```bash
export PKG_CONFIG_PATH=$PWD/build/ffmpeg-server/lib/pkgconfig
export LD_LIBRARY_PATH=$PWD/build/ffmpeg-server/lib
cargo build --release --bin ovsd --bin ovs-renderer
```

A unit needs the same at runtime, because the libraries are not in
`/usr/lib`:

```ini
Environment=LD_LIBRARY_PATH=/opt/openvideoserver/ffmpeg/lib
```

Two things that catch people, both of which caught this:

- **Check a shared build against its own libraries.** `./ffmpeg -encoders` run
  without `LD_LIBRARY_PATH` loads the machine's `libavcodec.so`, so it lists
  the machine's codecs while printing your configuration line. `--check` sets
  it; a shell does not.
- **Do not move a build after installing it.** The `.pc` files a linker reads
  have the prefix compiled into them, so a build installed to one directory and
  copied to another points at a directory that is not there.

Whether a release should ship this or Debian's is not decided. What is decided
is that it exists, that the server builds and passes its tests against it, and
that its preview encoder falls back from the card to `libopenh264` with no GPL
component anywhere in it.

## What is not here yet

- **Debian packaging.** These are unit files, not a `.deb`. Packaging comes with
  the release process.
- **A hardware watchdog.** systemd restarts a process that exits; it does not
  notice one that is still running and no longer presenting. The renderer
  already measures that as dropped frames and reports it, so the missing piece
  is a policy about what to do, not a way to see it.
- **`display` backend validation.** Direct display output through
  `VK_KHR_display` or DRM-KMS needs a machine with no compositor holding the
  device, and it cannot be validated on the development machine (R12).
  `OVS_RENDERER_BACKEND=display` is what the unit sets; until that path is
  proven on the reference server, `headless` is what runs everywhere else.
