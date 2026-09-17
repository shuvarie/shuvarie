# NSIS installer

`installer.nsi` builds the Windows installer for shuvarie using NSIS 3 with the
Modern UI 2 (MUI2) wizard and the bundled `MultiUser.nsh` mixed-mode header.

## Pages

1. **Welcome**
2. **License** — the MIT license; *Next* stays disabled until
   "I accept the terms in the License Agreement" is checked
3. **Choose Users** — *Install for anyone using this computer* (per-machine,
   requires elevation) or *Install just for me* (per-user)
4. **Installation Folder** — default `$PROGRAMFILES64\shuvarie` (per-machine)
   or `%LOCALAPPDATA%\Programs\shuvarie` (per-user)
5. **Additional Tasks** — checkbox to add the install folder to `PATH`
6. **Install** — installs `shuvarie.exe`, `LICENSE`, `README.md` and an
   uninstaller, registers an Add/Remove Programs entry and updates `PATH`
   (HKLM for per-machine, HKCU for per-user) when the checkbox is ticked
7. **Finish** — checkbox to launch shuvarie plus a link to the repository

The uninstaller (`Uninstall.exe`, also reachable through Apps & Features)
reverses all of the above: it removes the files, the ARP entry, and the
`PATH` entry, leaving user data untouched.

## Building

Requires [NSIS 3](https://nsis.sourceforge.io) (Unicode build) and a release
binary. From this directory:

```sh
makensis /DVERSION=<version> /DBINARY="..\target\x86_64-pc-windows-msvc\release\shuvarie.exe" installer.nsi
```

The script compiles against the files relative to its own directory
(`makensis` switches to it), so `..\LICENSE` and `..\README.md` are picked up
from the repository root. `/DBINARY` is expected to be an
`x86_64-pc-windows-msvc` build of the `shuvarie` crate.

### Configuration switches

| Switch | Default | Description |
| --- | --- | --- |
| `/DVERSION` | Crate version | Version string; must be three components (x.y.z) |
| `/DBINARY` | `../target/release/shuvarie.exe` | Path to the release binary |
| `/DOUT_FILE` | `shuvarie-setup-<version>.exe` | Installer output path |
| `/DAPP_NAME` | `shuvarie` | Product name used everywhere |
| `/DEXE_FILE_NAME` | `shuvarie.exe` | File name the binary gets in `$INSTDIR` |
| `/DPUBLISHER` | `Charles Dong` | ARP publisher / version-info company |
| `/DAPP_URL` | repository URL | Shown on the finish page, ARP entries |

On POSIX (cross-compiling with the Linux `makensis` package) use `-D` switches
and forward slashes in paths, e.g. `-DBINARY=../target/x86_64-pc-windows-msvc/release/shuvarie.exe`.

## Silent installation

```
shuvarie-setup.exe /S [/AllUsers | /CurrentUser]
```

`/AllUsers` and `/CurrentUser` pick the installation mode without the Choose
Users page. A silent install always adds shuvarie to `PATH` (there is no task
page), so a per-machine silent install needs an elevated shell.

## Implementation notes

- **PATH editing** is done by reading the raw `REG_EXPAND_SZ` value, appending
  with `WordFunc`'s case-insensitive `WordAdd` (deduplicating existing
  entries), and writing it back with `WriteRegExpandStr`, followed by a
  `WM_SETTINGCHANGE` broadcast so running processes pick it up. If the value
  is longer than `NSIS_MAX_STRLEN - 256` characters it is left untouched and
  the user is asked to add the folder manually.
- **Unattended mode restores the right mode**: the installer writes an
  `install-mode` marker file next to the uninstaller; `un.onInit` uses it to
  pick the correct registry hive even when per-machine and per-user copies
  are installed side by side.
- **64-bit registry**: the per-machine ARP entry is written to (and removed
  from) the native registry view when running on x64, independent of whether
  the installer itself was built as `x86-unicode` or `amd64-unicode`.
- `SetRegView` aside, the script is arch-agnostic; building with
  `makensis /TARGETARCH=amd64` (Windows NSIS distribution only) produces a
  64-bit installer.
