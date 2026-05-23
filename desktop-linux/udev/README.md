# Granting the Linux receiver permission to open `/dev/uinput`

The zerowire receiver creates a virtual mouse / keyboard via the Linux
kernel's [`uinput`](https://www.kernel.org/doc/html/latest/input/uinput.html)
character device. By default `/dev/uinput` is `crw------- root:root`, so
running `zerowire-cli receive` will fail with:

```
Error: opening /dev/uinput (need root or `uinput` group + udev rule; see desktop-linux/udev/README.md)

Caused by:
    Permission denied (os error 13)
```

You have two options.

## Option A — quickest: just run as root

```bash
sudo modprobe uinput
sudo ./target/debug/zerowire-cli receive --simulate
```

Good for trying the demo once. Don't run a long-lived daemon this way.

## Option B — recommended: udev rule + `uinput` group

This is one-time setup. After it, `zerowire-cli` runs as your normal user.

```bash
# 1. Make sure the kernel module loads on boot.
sudo modprobe uinput
echo uinput | sudo tee /etc/modules-load.d/zerowire.conf

# 2. Create the uinput group and add yourself.
sudo groupadd -f uinput
sudo usermod -aG uinput "$USER"

# 3. Install the udev rule (in this directory).
sudo install -m 0644 ./99-zerowire-uinput.rules \
    /etc/udev/rules.d/99-zerowire-uinput.rules
sudo udevadm control --reload-rules
sudo udevadm trigger

# 4. Log out and log back in so the new group membership takes effect.
#    (Or open a new shell with `su - $USER`.)

# 5. Verify.
ls -l /dev/uinput     # should be crw-rw---- root:uinput
groups | grep uinput  # should include uinput
./target/debug/zerowire-cli receive --simulate   # no sudo needed
```

If `ls -l /dev/uinput` still shows mode `600` after the rule reload, your
system's `systemd-tmpfiles` may be re-creating it; reboot once.

## Why not just use root?

* You'd be running networked code as root the whole session.
* The uinput device persists across sessions if you set it up via udev, so
  unprivileged tools (`evtest`, `libinput debug-events`, the GNOME settings
  panel) can also see it.
* Apt/dnf packages of zerowire will ship this rule preinstalled — this dir
  is the source of truth for what they install.
