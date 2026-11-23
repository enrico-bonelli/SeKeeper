# Sekeeper

> **[Leggi in italiano](README.it.md)**

<img src="data/icons/hicolor/scalable/apps/io.github.sekeeper.Sekeeper.svg" width="104" height="104" align="right" alt="Sekeeper icon">

**Desktop password manager for Linux with an encrypted vault and optional AWS S3
synchronization.**

Sekeeper is a lightweight desktop application written in Rust with a GTK4 interface.
It is designed to keep personal credentials in a dedicated store protected by a
**master password and a key file**, with simple day-to-day ergonomics: open the
vault, search for an account, view or update its details, and lock the application
when you are done.

The vault stays on your computer and works offline. No Sekeeper account or dedicated
server is required. Optional integration with your own AWS S3 bucket adds versioned
encrypted copies, multi-PC synchronization, and recovery on a new installation. Cloud
configuration remains entirely under your control. The interface automatically uses
**Italian** when an Italian locale is active in the environment (`LC_ALL`,
`LC_MESSAGES`, `LANGUAGE`, `LANG`) and falls back to **English** otherwise.

## What you can do

- **Store credentials** with description, username, password, address, and notes.
- **Search by description, username, or address**, partially and case-insensitively,
  with **Ctrl+F** and **Esc** shortcuts.
- **Copy usernames and passwords without revealing them**: the clipboard clears
  automatically after 30 seconds and when the vault locks.
- **Generate random passwords** directly inside the editor.
- **Import and export CSV**, including to and from other password managers.
- **See when each entry was created and last modified**, in the details pane and editor.
- **Protect access with two independent factors**: the master password you remember
  and a random key file you store separately.
- **Recover previous states** through up to ten encrypted local backups, each with its
  timestamp.
- **Use the same vault across multiple Linux PCs**, with optional S3 sync, offline
  editing, and explicit conflict resolution.
- **Lock the vault manually or automatically** after five minutes of inactivity;
  light/dark theme follows your desktop preference.

## Contents

- [Requirements and building](#requirements-and-building)
- [Creating your first vault](#creating-your-first-vault)
- [Daily use](#daily-use)
- [Importing and exporting](#importing-and-exporting)
- [Safeguarding the key file](#safeguarding-the-key-file)
- [Local backups and recovery](#local-backups-and-recovery)
- [AWS S3 synchronization (optional)](#aws-s3-synchronization-optional)
- [Security and data format](#security-and-data-format)
- [Current limitations](#current-limitations)
- [Development checks](#development-checks)

## Requirements and building

### Source layout

Sekeeper consists of two projects that must sit **in the same parent directory**:

```text
src/
├── sekeeper/        this desktop application
└── sekeeper-core/   shared library used by desktop and Android
```

[`sekeeper-core`](../sekeeper-core) contains the vault format (Argon2id, AES-256-GCM,
and key file), revision history reconstruction, credential model, CSV import/export,
phone setup QR payload, and EN/IT localization. It is not published on crates.io:
`Cargo.toml` references it via a local path dependency (`../sekeeper-core`), so builds
fail immediately if that directory is missing. Always keep both repositories on the
same version so vault format and history match across devices.

### Installing on Arch Linux

The local package recipe is in [`packaging/arch/PKGBUILD`](packaging/arch/PKGBUILD).
From a complete checkout with `sekeeper-core` in the sibling directory (see
[Source layout](#source-layout)):

```sh
cd packaging/arch
makepkg -si
```

Run `makepkg` as a regular user. It installs build dependencies, compiles the
application, runs the standard test suite, and prompts to install via `pacman`. The
recipe packages both this project and `../sekeeper-core`, so local changes to either
are included. You need `base-devel` and a Rust toolchain (`rustup` works if its
default toolchain is compatible). Network access is needed on the first build to
fetch Cargo dependencies.

The package installs `sekeeper` in `/usr/bin`, the **Sekeeper** desktop entry, the
icon in the `hicolor` theme, plus README and license. The icon is also embedded in
the binary for running from a development tree. Personal data in
`~/.config/sekeeper/` and your key file are never part of the package. The recipe
targets Arch Linux x86_64 using local sources; it is not an AUR package and does not
require a GitHub URL.

To build the package without installing, run `makepkg -s` and install later with:

```sh
sudo pacman -U ./sekeeper-0.3.1-2-x86_64.pkg.tar.zst
```

The optional dependencies `curl` and `libsecret` (with an active Secret Service
keyring) are needed for S3 sync; they are not required for local-only use.

### Manual compilation

To build from source you need:

- **Linux** and **Rust 1.89 or later** (edition 2024);
- the [`sekeeper-core`](#source-layout) library in `../sekeeper-core`;
- **GTK4 4.6 or later** development libraries, plus `pkg-config` and
  `glib-compile-resources` to embed the icon.

On Arch Linux:

```sh
sudo pacman -S --needed base-devel rust gtk4 glib2 pkgconf
```

On Debian/Ubuntu the equivalent packages are `build-essential`, `libgtk-4-dev`, and
`pkg-config` (install Rust via `rustup` if the distribution package is older than
1.89). No cloud services are required for local use.

For optional AWS S3 sync you also need `curl` 7.75 or later with AWS SigV4 support
and `secret-tool` (`libsecret` on Arch Linux, `libsecret-tools` on Debian/Ubuntu)
with an active Secret Service keyring in your session. Sekeeper invokes the ELF
binaries at `/usr/bin/curl` and `/usr/bin/secret-tool`: they must belong to the
system tree owner, with no symlinks and no group/other write permissions. `/proc`
must be mounted, as on standard Linux desktops.

Build and run from the parent directory containing both projects:

```sh
ls sekeeper-core/Cargo.toml   # shared library must exist
cd sekeeper
cargo build --locked --release
./target/release/sekeeper
```

`--locked` uses the exact dependency versions pinned in `Cargo.lock`, including
`sekeeper-core`. Network access is required on the first build; subsequent builds
work offline. Cargo recompiles `sekeeper-core` automatically whenever its sources
change. Release builds enable LTO and take a few minutes.

For faster development builds, use `cargo run --locked`. Even in debug builds,
Argon2id is compiled with optimizations so unlocking stays responsive. After
rebuilding, close any running instance before launching the new binary, as a second
invocation may simply focus the existing process.

### Appearance and desktop integration

Light/dark mode follows the desktop preference through the XDG portal, including live
changes at runtime. On startup Sekeeper also checks GTK4 settings and, on KDE, the
window palette as a fallback when the portal is unavailable. An explicit `GTK_THEME`
is respected. Key file choosers are internal GTK dialogs and do not require the
desktop file chooser portal.

## Creating your first vault

1. Launch Sekeeper and choose a unique **master password** of at least 15 characters,
   preferably a passphrase of 5–6 randomly chosen words.
2. Click **Generate key file…** and save it outside the vault directory — for example
   on an already mounted encrypted USB drive. If you already have a key file
   generated by Sekeeper, use **Choose key file…**.
3. Keep an **identical recovery copy of the key file** in a second, separate location.
4. Confirm the password and click **Create vault**.
5. Click **Add** to store your first credential. Description, username, and password
   are required; address and notes are optional.

**Both master password and key file are required.** The key file is not a one-time
code: keep and reuse it rather than regenerating it on every launch. If you forget
the password or lose all copies of the key file, there is no recovery mechanism.

If S3 is already configured and you want to start with an empty vault, choose
**Create a new empty vault** from the recovery screen. If the vault already exists on
another PC, follow [Recovering the vault on a new PC](#recovering-the-vault-on-a-new-pc).

## Daily use

At launch, enter your master password, select the key file, and click **Unlock**. The
key file path is remembered during the current session. With **Remember path on this
PC** enabled, the path is remembered across restarts as well; otherwise you select it
again each time. You may unplug the USB drive after unlocking and continue working
until the next lock.

The list on the left is sorted alphabetically. Selecting an entry shows its details
on the right with the password hidden: the eye icon reveals it and the copy icon
places it on the clipboard without displaying it. Use **Add**, **Edit**, and
**Delete** to manage entries; double-clicking a row opens the editor. **Lock** closes
the session and requires both factors again to unlock.

### Clipboard

The clipboard is cleared automatically after 30 seconds and on lock, unless you have
copied something else in the meantime. Copied entries carry the
`x-kde-passwordManagerHint` marker asking Klipper not to store them in clipboard
history; other clipboard managers may ignore this hint.

### Generating a password

Inside the editor, **Generate** fills the password field with a random password of
the chosen length (20 characters by default, from 8 to 64). It always includes
lowercase, uppercase, digits, and symbols while avoiding visually ambiguous
characters such as `I`, `l`, `1`, `O`, and `0`.

### Search and timestamps

The search bar filters entries by description, username, or address (partial,
case-insensitive matching). Passwords and notes are never searched. **Ctrl+F**
focuses search; **Esc** clears it. The counter displays matching entries versus total
entries. Editing and deleting apply to the selected entry even while filtered.

Creation and last-modified timestamps appear in the details pane and editor in local
time. They are stored inside the encrypted vault and its copies, including S3 copies.
Creation time never changes; last-modified updates only when entry data changes.
Saving without changes does not create a new revision.

Missing timestamps display **Unavailable**. A subsequent edit records the
modification timestamp without inventing a creation date. All PCs must run a version
with key file support.

### Auto-lock

The vault locks automatically after five minutes of inactivity inside the app, even
if an editor dialog is open. Locking closes open editors and discards unsaved
changes. When supported by desktop services, Sekeeper also locks on session lock,
session deactivation, and before suspend via ScreenSaver and systemd-logind D-Bus
signals. The idle timeout works even without those services.

## Importing and exporting

The menu next to **S3…** provides **Import from CSV…** and **Export to CSV…**.

**Exporting creates an unencrypted file**: anyone who reads it sees every password.
The file is created with `0600` permissions, never overwrites existing files, and
requires a user-owned directory not writable by others. Store it on encrypted media
and delete it as soon as you finish.

Import matches columns by header name (supporting both English and Italian headers),
so it accepts Sekeeper exports as well as CSVs from applications such as Chrome,
Bitwarden, and KeePassXC. At least a description column and a password column are
required, and the file must be UTF-8. Entries identical to an existing one are
skipped.

To change your master password or key file: export to CSV, create a new vault in a
fresh installation or after archiving the old one, import the CSV, and delete the CSV
file. Entry timestamps are preserved. When using S3, choose a new prefix because the
new vault is independent of previously uploaded copies.

## Safeguarding the key file

The key file is a second secret required to open the vault. You can keep it on media
separate from your computer and from S3 copies. Generating a key file in Sekeeper
never overwrites an existing file.

The file holds a random 256-bit secret; both factors are cryptographically required
to derive the encryption key. Removing a UI check cannot open the vault without the
file, and there is no password-only fallback. Key derivation uses secret input `K` as
specified by [Argon2, RFC 9106](https://www.rfc-editor.org/rfc/rfc9106.html#section-3.1).

For every unlock or recovery, select the key file and enter the master password. The
path is remembered for the current app session. Enabling **Remember path on this PC**
saves the path in plaintext in `vault.prefs.json` after a successful unlock: it only
stores the file location, never its contents, though anyone able to read your files
learns where you keep it. Once unlocked, you may unplug the drive: the app holds only
the derived key until lock, allowing saves and synchronization without re-reading the
key file.

Keep an **identical recovery copy** on separate media. You can move or rename the key
file without affecting the vault; generating a new file with the same name produces a
different key and will not open the vault. If you forget the master password or lose
all copies of the key file, the data cannot be recovered. Changing the key file on an
existing vault in place is not supported.

The key file contents are never uploaded to S3 or stored in the vault, backups,
configuration, or keyring. You may keep an externally encrypted copy in another cloud
storage service and decrypt it to a local file before use. Sekeeper does not encrypt
the key file itself, mount drives, or decrypt external containers.

### Setting up the phone

With the vault unlocked and the key file medium connected, the menu next to **S3…**
offers **Set up phone…**. Sekeeper re-reads and verifies the key file and displays a
QR code for [Sekeeper for Android](../sekeeper-android), along with bucket, region,
and prefix if S3 is configured. You can include this PC's AWS keys (read from the
keyring) or enter dedicated phone keys (which are not saved on the PC). Reusing the
PC's keys means PC and phone share one IAM identity: revoking it disconnects both.
Dedicated phone keys — needing only `s3:ListBucket` and `s3:GetObject` while the
phone app is read-only — can be revoked independently if the phone is lost. Temporary
credentials with a session token cannot be transferred. The dialog closes
automatically after 60 seconds or when the vault locks.

The master password is never part of the QR code. The key file and AWS keys are:
anyone photographing the code gets everything except the master password. Display it
only where nobody else can see your screen. Linux does not allow applications to
block screenshots.

### Storage media and permissions

The key file must belong to your user and have `0600` (or stricter) permissions;
symlinks and hard links are rejected. Path components must also pass ownership and
writability checks. The target directory must already exist, belong to your user, and
not be group/other writable; Sekeeper does not alter directory permissions. Use a
filesystem supporting Unix permissions or equivalent mount options. Secret buffers
are zeroized after key derivation.

## Local backups and recovery

Files live in `~/.config/sekeeper/`:

| File | Contents |
| --- | --- |
| `vault` | Current encrypted vault |
| `vault.v3.bak.1` | State before the last save |
| `vault.v3.bak.2` through `vault.v3.bak.10` | Up to nine earlier states |
| `vault.v3.before-restore` | Current file preserved before the last restore |
| `vault.lock` | Lock file coordinating writes; contains no passwords |
| `vault.s3.json` | Local S3 configuration, without AWS credentials |
| `vault.prefs.json` | Optional: path to the key file, never its contents |
| `vault.history-v3/` | Encrypted revision history, written only when S3 is active |
| `vault.s3-cache/` | Downloaded encrypted copies, isolated per S3 destination |

Backups are encrypted identically to the vault and rotate before every save (a newly
created vault has no previous backups yet). They protect against accidental edits or
deletions, but live on the same disk: keep external copies as well and verify a copy
before replacing it. Lock or close Sekeeper before copying files to get a consistent
snapshot.

Deleting a credential removes it from the current vault, not from historical local or
S3 copies that already contain it.

### Restoring a local backup

To restore a previous state:

1. Lock the vault if it is open.
2. Click **Restore a previous copy…** on the unlock screen.
3. Pick a backup by timestamp, enter its master password, and select the key file.
4. Confirm the restore. Changes made after that backup will be undone.

Restore works even if `vault` is missing or corrupted. The chosen backup is
authenticated and read before replacing the current file. A wrong password, wrong key
file, or damaged backup leaves the current vault untouched. If backup copies exist,
the app offers recovery instead of creating an empty vault.

If a durability error is reported after writing, the file already contains the edits:
Sekeeper keeps the same in-memory state without silently retrying, though survival
across a sudden power loss is not confirmed.

## AWS S3 synchronization (optional)

S3 lets you keep off-device copies of your vault and share the same archive across
multiple Sekeeper installations. Each synchronized save produces an encrypted copy
identified by a revision ID; S3 never receives your master password or key file.

### Configuring the first PC

**S3…** opens sync settings (disabled until configured). Enter bucket, region (e.g.
`eu-central-1`), prefix (e.g. `sekeeper/personal/`), PC name, and dedicated IAM
credentials. The vault key file is never uploaded to S3: transfer an identical copy
to your other PCs through a separate channel. Session token is optional for temporary
credentials; when they expire, enter fresh credentials in settings. Leaving all
credential fields blank preserves the previously saved keys.

AWS credentials are stored exclusively in your local keyring via `secret-tool`,
separate from the master password. They are never written to JSON config, the vault,
or S3 objects. If the keyring is unavailable, Sekeeper reports an error and never
falls back to plaintext credential files. When replacing credentials, the previous
keyring item is deleted after saving the new config. Use the same bucket and prefix
on every PC. Dedicated per-device credentials are recommended so each device can be
revoked individually; the phone may also receive the PC's keys (see
[Setting up the phone](#setting-up-the-phone)). Never use root or administrator AWS
credentials.

### Bucket and IAM permissions

The bucket must be private with all Block Public Access settings enabled, ACLs
disabled, and SSE-S3 encryption enabled. Enabling **S3 Versioning** is recommended.
Start with object lifecycle expiration rules disabled.

Anyone who obtains the bucket's write/delete keys cannot read or forge vault data,
but can delete copies. An already-synced PC — and the phone, which keeps the latest
opened version — refuses silent rollback to an older revision and alerts you. A brand
new PC performing recovery, however, can only see what remains in the bucket. With S3
Versioning enabled, deleted copies remain recoverable as non-current versions; for
stronger protection consider **MFA Delete** or **S3 Object Lock** in governance mode
on the vault prefix.

This IAM policy is sufficient for desktop sync; replace `BUCKET_NAME`. The
`s3:DeleteObject` statement uses the example prefix `sekeeper/personal/`: adjust it
to match your configured prefix while keeping the `v3/versions/*` suffix. Upgrading
from 0.1.0 only adds this delete permission (without it, uploads and reads still work
while retention pruning stays pending). Attach it to a dedicated identity with no
broader S3 policies:

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": "s3:ListBucket",
      "Resource": "arn:aws:s3:::BUCKET_NAME",
      "Condition": { "StringLike": { "s3:prefix": "sekeeper/*" } }
    },
    {
      "Effect": "Allow",
      "Action": "s3:GetObject",
      "Resource": "arn:aws:s3:::BUCKET_NAME/sekeeper/*"
    },
    {
      "Effect": "Allow",
      "Action": "s3:PutObject",
      "Resource": "arn:aws:s3:::BUCKET_NAME/sekeeper/*",
      "Condition": { "StringEquals": { "s3:if-none-match": "*" } }
    },
    {
      "Effect": "Allow",
      "Action": "s3:DeleteObject",
      "Resource": "arn:aws:s3:::BUCKET_NAME/sekeeper/personal/v3/versions/*"
    }
  ]
}
```

### Retaining the latest 30 full copies

Since Sekeeper **0.2.0**, each successful sync retains at most **30 full copies**
under the S3 prefix, always including the current state. Later revisions in the
verified ancestry graph take precedence, using authenticated timestamps to order
equivalent branches. A skewed PC clock therefore cannot make an older revision
override a descendant edit, nor do S3 upload times or alphabetical object names
decide the current head.

Each vault carries an encrypted summary of ancestry links (IDs and parent relations
only, with no old passwords or entry contents), enabling an offline PC to reconnect
even after its original snapshot has been pruned from S3. The first cleanup
automatically consolidates existing v3 history without changing passwords or key
files.

The snapshot carrying the updated lineage summary must be uploaded and read back
successfully before any older object is deleted. After S3 pruning, superseded files
in that PC's local cache and sync history are cleaned up as well. The ten local
recovery backups remain separate.

While conflicts are open, pruning pauses and the copy count may exceed 30 until the
conflict is resolved. Concurrent transfers, network errors, or missing permissions
simply defer cleanup to future syncs while keeping the local vault usable.

**Update Sekeeper on every PC before enabling 0.2.0+ sync.** Older builds do not
understand the lineage summary and cannot sync a pruned history.

The initial sync still downloads and verifies existing history; subsequent syncs work
only on the remaining full copies. Do not configure expiration rules on current
objects — Sekeeper manages retention itself.

With **S3 Versioning** enabled, `DeleteObject` inserts a delete marker while physical
non-current versions may continue incurring storage costs. The 30-copy limit applies
to current objects listed by Sekeeper. To expire non-current versions, configure a
separate `NoncurrentVersionExpiration` lifecycle rule scoped strictly to the vault
prefix. Sekeeper never modifies bucket rules and does not require
`s3:DeleteObjectVersion`.
[Deleting object versions in S3](https://docs.aws.amazon.com/AmazonS3/latest/userguide/DeletingObjectVersions.html).

### How synchronization works

With the example prefix, copies are stored at
`sekeeper/personal/v3/versions/<random-id>.sek`. Each copy is a complete encrypted
vault whose revision ID, parent IDs, device name, and timestamp are protected by the
same authenticated encryption. Timestamps are for display, not head selection.

Sync runs on unlock, after saves, and every minute while the vault is open, or on
demand via **S3… → Sync now**. Networking and keyring calls run on worker threads. An
in-flight request may finish after the vault locks, but cannot install a remote copy
into a locked or subsequently reopened session.

- If the remote copy descends from the local one, it is installed after cryptographic
  verification and local backup. Installation is deferred while an editor dialog is
  open, and concurrent local edits are re-checked.
- If diverging versions exist, both are preserved and published. Open **S3… →
  Versions and conflicts…** and explicitly choose which version to keep. Each version
  shows which entries would be recovered, lost, or modified relative to your current
  vault. Your choice creates a merge revision recognizing both parents (no automatic
  per-credential merge). Remaining historical copies stay recoverable from the same
  list.
- Without network access or with AWS access denied, the local vault keeps working;
  edits remain in encrypted history and are retried later. A history preparation
  error after saving does not roll back an already-saved local vault edit.
- Remote copies are authenticated before publishing local data to a non-empty
  destination. A foreign vault under the same prefix, tampered file, or incomplete
  history halts sync. Leftover local copies from a deleted and recreated vault are
  ignored.

### Recovering the vault on a new PC

On a **new PC**, configure S3 from the initial screen and click **Recover from
S3…**, supplying the master password and an identical copy of the original vault's
key file. Do not create a second independent vault under the same prefix. If
diverging versions exist, choose which one to recover. Recovery never overwrites an
existing local vault or backups: if a local file is damaged, try local backups first;
otherwise back up `vault` and `vault.v3.bak.*` elsewhere before starting S3 recovery.

If the v3 bucket is still empty, click **Create a new empty vault** on the recovery
screen to start fresh without disabling S3, then recover that vault on other PCs. The
IAM policy on `sekeeper/*` remains valid.

### Transport and history limits

Transport uses HTTPS with AWS SigV4 signing provided by `curl`. Helpers are validated
and launched by binary file descriptor, avoiding `PATH` lookups or TOCTOU path swaps.
They receive a minimal environment without loader, GIO, TLS certificate, or proxy
overrides. Credentials travel over a pipe, never in process arguments. Encrypted
upload bodies are copied into a write/resize-sealed `memfd` inherited only by the
upload helper without reopening disk cache paths. Every upload sends
`If-None-Match: *` so existing objects are never overwritten, and `412` responses are
verified by downloading the existing object. Pruning deletes only superseded IDs
belonging to the vault's authenticated history under its configured prefix.

Local caches avoid re-downloading previously fetched history. Downloaded copies enter
the cache only after authentication with the vault key and verification that the
contained revision ID matches the object name (or after password + key file
verification during new-PC recovery). A tampered or corrupted copy stays in memory,
causes sync to fail, and is re-fetched once the bucket is repaired; older caches are
cleaned on the first error. `curl` responses are capped while reading up to the copy
size limit even on `curl` versions that do not enforce `--max-filesize` on chunked
transfers. Each transfer step has a deadline, and interrupted uploads resume cleanly
without duplicating revisions.

Defensive limits cap snapshots at 16 MiB each, 2048 full copies, 64 MiB of encrypted
history processed, and 65,536 lineage links per vault. If a limit is hit, sync halts
while keeping the local vault usable. Regular 30-copy retention prevents unbounded
growth of full snapshots while preserving revision metadata for offline PCs.
Disabling S3 never deletes local or remote files.

## Security and data format

### Vault encryption

The current format is `SEKEEPER`, **version 3**, with mandatory key file. Earlier v1
and v2 formats are rejected with no migration path; create a new vault after
archiving old test vaults. Backups, history, cache, and S3 paths for v3 are separate
from earlier formats, while S3 configuration and keyring credentials can be reused.

- Argon2id version `0x13`, **256 MiB**, **3 iterations**, parallelism **4**;
- 32-byte random key file secret fed as Argon2id secret input `K`;
- 32-byte derived key, 16-byte random salt;
- AES-256-GCM with 12-byte random nonce and 16-byte authentication tag;
- Encrypted JSON payload for all credential fields;
- 52-byte header authenticated in full as AES-GCM AAD.

The header carries magic, format version, KDF/Argon2/cipher identifiers, Argon2 cost
parameters as three little-endian 32-bit integers, salt, and nonce. Only the exact v3
profile is accepted: headers with weaker, excessive, or unknown parameters are
rejected before running the KDF. The profile does not depend on library defaults.
Argon2 working memory is zeroized even on error; detectable allocation failures
return an error without falling back to weaker parameters.

### File and process hardening

The vault, backups, and lock file are created with `0600` permissions inside a `0700`
directory using temporary files and atomic replacement. External modifications are
detected before saving so you must lock and reopen rather than silently overwriting
other changes.

Directories are traversed one component at a time using `openat` and `O_NOFOLLOW`,
and reads, writes, renames, and temporary cleanup operate on open directory file
descriptors. Symlinks, non-regular files, hard links, unexpected owners, and
group/other accessible files are rejected. Data paths containing symlinks (including
intermediate components) are not supported. Directory creation is synced in the
parent directory as well.

At startup, before initializing GTK or loading secrets, the process sets
`PR_SET_DUMPABLE=0`, soft and hard `RLIMIT_CORE=0`, `PR_SET_NO_NEW_PRIVS=1`, and
`umask 077`, exiting immediately if any protection cannot be enabled. Helpers inherit
core dump disablement and `NO_NEW_PRIVS`.

Rust-managed buffers for master password, derived key, credentials, plaintext JSON,
and Argon2 working memory are zeroized on drop, with cryptographic crate zeroize
features enabled. This minimizes residual copies without guaranteeing the absence of
transient copies created by GTK, the compiler, or the OS. Encryption protects the
vault at rest; it does not defend against malware observing an already-unlocked
session.

### Key file format

`SEKEY001` (8 bytes) + random secret (32 bytes) + SHA-256 checksum of the first 40
bytes (32 bytes) = 72 bytes total. The checksum detects accidental corruption; it
does not encrypt the file.

## Current limitations

Sekeeper keeps its scope intentionally focused:

- Desktop application for **Linux**. [Sekeeper for Android](../sekeeper-android)
  currently views the vault read-only. Browser extensions and autofill are out of
  scope.
- No TOTP/Authenticator unlock: the vault's second factor is the key file.
- No in-place master password or key file rotation: use
  [export and import](#importing-and-exporting) into a new vault.
- Sync links copies of the **same vault**. Conflicts require picking a complete
  revision rather than merging individual credentials.
- S3 cleanup keeps 30 full copies and pauses deletion during conflicts. The encrypted
  lineage summary remains subject to the limits above; pruned copies can no longer be
  restored from the app's list.
- Legacy v1 and v2 test formats are not opened by v3, with no migration path.

Vault security relies on a strong master password and separate custody of the key
file. Encrypted backups help recover vault data, but never replace a recovery copy of
the key file.

## Development checks

The repository includes sources, `Cargo.lock`, graphic resources, Arch recipe, and
public test fixtures. `.gitignore` excludes build artifacts, generated packages,
local config, personal vaults, and personal key files. The exception
`tests/fixtures/public-test.key` is strictly for tests and must be kept. Keep
`Cargo.lock` to reproduce verified dependency versions.

```sh
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
```

After modifying `sekeeper-core`, run its test suite too and then re-run this
project's tests. The `testing` feature of `sekeeper-core` exposes public test keys
for fixtures: this crate enables it only under `[dev-dependencies]` and must never
enable it for normal builds.

```sh
cd ../sekeeper-core
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

The GUI test requires a display and is excluded from default runs. Use Xvfb or
`kwin_wayland --virtual` to avoid opening desktop windows:

```sh
xvfb-run -a cargo test --locked ui::tests -- --ignored
```

It exercises key file generation/chooser dialogs, vault creation and mandatory key
file unlock, saving with the key file drive disconnected, search and filtered
operations, details pane, clipboard copy/clear, password generator, remembered key
file path, editor scrubbing on idle lock, and backup restore. Other tests verify key
file generation/integrity/permissions, wrong second factor rejection, search,
timestamps, encryption, tampering, legacy format rejection, permissions, backups,
concurrency, write errors, process dump/privilege isolation, helper binary checks,
sealed `memfd` uploads, hostile filesystem setups, and 30-copy retention/recovery/
offline/conflict scenarios. Default tests never touch real AWS objects.

The fixture `tests/fixtures/vault-v3` contains only public dummy data encrypted with
password `public fixture master password` and `tests/fixtures/public-test.key`
(32 bytes of `0x42`). Never use these values for a real vault. The v1 and v2 fixtures
verify rejection of legacy formats.

### Live AWS S3 integration test

Standard tests use an in-memory service and a mock transport; they never read real
AWS credentials or contact real buckets. The live test is ignored by default. To run
it, export these environment variables without committing them:

- `SEKEEPER_S3_TEST_BUCKET`
- `SEKEEPER_S3_TEST_REGION`
- `SEKEEPER_S3_TEST_ACCESS_KEY_ID`
- `SEKEEPER_S3_TEST_SECRET_ACCESS_KEY`
- `SEKEEPER_S3_TEST_SESSION_TOKEN` (only if required)

```sh
cargo test --locked s3::tests::aws_s3_live_roundtrip_and_conditional_upload -- --ignored
```

This test uploads only a dummy vault with the public test password and key file under
a fresh `sekeeper/integration-test-…/` prefix, verifying read, list, and overwrite
protection. It leaves the object in the bucket because the test identity needs no
delete permission. This is a live AWS operation subject to AWS pricing.

## License

Code and icon are distributed under the [MIT License](LICENSE).
