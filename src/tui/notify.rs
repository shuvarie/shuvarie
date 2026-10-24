//! Notification sound playback: the audio half of the desktop notice a
//! finished, failed, or user-blocked turn raises.
//!
//! The sound is played by handing the file to whichever audio player the host
//! has, so no audio decoding happens in-process and no audio crate is dragged
//! in. A notification carries an ordered list of candidates rather than a single
//! file, because a default sound is only ever as present as the host's own sound
//! theme; the first candidate that exists is the one played. Playback is strictly
//! best-effort: no candidate existing, or a host with no player at all, is simply
//! silent.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One candidate audio player, in the order they are tried.
struct Player {
    program: &'static str,
    args: &'static [&'static str],
}

/// Placeholder an entry's `args` may use to embed the sound path *inside* an
/// argument, for a player that takes the path as part of a command string
/// rather than as an argument of its own (see the Windows entry). An entry that
/// uses it does not also get the path appended as a trailing argument.
const FILE_PLACEHOLDER: &str = "{file}";

/// The players tried on Unix systems other than macOS. `paplay` and `pw-play`
/// are the PulseAudio and PipeWire sound servers, and `ffplay` and `mpv` decode
/// anything `ffmpeg` understands — which covers the Ogg Vorbis default.
#[cfg(all(unix, not(target_os = "macos")))]
const PLAYERS: &[Player] = &[
    Player {
        program: "paplay",
        args: &[],
    },
    Player {
        program: "pw-play",
        args: &[],
    },
    Player {
        program: "ffplay",
        args: &["-nodisp", "-autoexit", "-loglevel", "quiet"],
    },
    Player {
        program: "mpv",
        args: &["--no-video", "--really-quiet"],
    },
];

/// macOS ships `afplay`, which decodes the `.aiff` alert tones the macOS
/// default points at, so it is the only candidate there.
#[cfg(target_os = "macos")]
const PLAYERS: &[Player] = &[Player {
    program: "afplay",
    args: &[],
}];

/// Windows has no command-line audio player, but `System.Media.SoundPlayer` is
/// available in the PowerShell that ships with the OS (5.1 and later), and it
/// plays the `Media` wavs the Windows default points at. `PlaySync` keeps this
/// short-lived player process alive until the sound finishes — the process is
/// detached, so nothing on our side waits on it.
#[cfg(windows)]
const PLAYERS: &[Player] = &[Player {
    program: "powershell",
    args: &[
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "(New-Object Media.SoundPlayer '{file}').PlaySync()",
    ],
}];

/// Neither a player nor (see `shuvarie_config::DEFAULT_NOTIFY_SOUNDS`) a default
/// sound on platforms that are neither Unix nor Windows.
#[cfg(not(any(unix, windows)))]
const PLAYERS: &[Player] = &[];

/// Play the first of `files` that exists, for a finished turn, detached so the
/// render loop never waits on it: the player is spawned with its streams closed,
/// and a short-lived thread reaps it so finished players don't pile up as
/// zombies.
///
/// Nothing is played when no candidate exists — the notification then stays
/// silent instead of surfacing a player error — and a leading `~` in a
/// configured path expands to the home directory.
pub fn play_sound(files: &[PathBuf]) {
    let Some(path) = resolve_sound(files) else {
        return;
    };
    for player in PLAYERS {
        let mut cmd = Command::new(player.program);
        cmd.args(player_args(player, &path));
        if let Ok(mut child) = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return;
        }
    }
}

/// The full argument list for `player`: its own arguments, with every
/// [`FILE_PLACEHOLDER`] replaced by `path`, plus `path` appended as a trailing
/// argument when the entry did not embed it.
///
/// A substituted path is escaped for the single-quoted PowerShell string it
/// lands in, by doubling `'` — a home directory can contain an apostrophe. No
/// entry on the other platforms embeds the path, so nothing else sees a
/// backslash-escaped path.
fn player_args(player: &Player, path: &Path) -> Vec<String> {
    let path = path.to_string_lossy();
    let mut args = Vec::with_capacity(player.args.len() + 1);
    let mut embedded = false;
    for arg in player.args {
        if arg.contains(FILE_PLACEHOLDER) {
            embedded = true;
            args.push(arg.replace(FILE_PLACEHOLDER, &path.replace('\'', "''")));
        } else {
            args.push((*arg).to_string());
        }
    }
    if !embedded {
        args.push(path.into_owned());
    }
    args
}

/// The files to try, in order, for a notification sound: the file a config
/// named, or the platform's own list when it named none. A named file replaces
/// that list rather than joining it, so naming a file pins exactly what plays.
pub fn candidates(configured: Option<&Path>, defaults: &[&str]) -> Vec<PathBuf> {
    match configured {
        Some(path) => vec![path.to_path_buf()],
        None => defaults.iter().map(PathBuf::from).collect(),
    }
}

/// The first candidate that is an existing file, after `~` expansion, or `None`
/// when there is nothing to play.
fn resolve_sound(files: &[PathBuf]) -> Option<PathBuf> {
    files
        .iter()
        .map(|path| expand_home(path))
        .find(|path| path.is_file())
}

/// Expand a leading `~` (bare, or followed by a separator) to the user's home
/// directory, so config files can spell a sound path portably. Any other path
/// — including `~user`, which needs the passwd database — is kept as written.
fn expand_home(path: &Path) -> PathBuf {
    expand_home_in(path, dirs::home_dir().as_deref())
}

/// [`expand_home`] with an explicit home directory, so tests can exercise the
/// expansion without reading the environment.
fn expand_home_in(path: &Path, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return path.to_path_buf();
    };
    match path.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_home_only_rewrites_a_leading_tilde() {
        let home = Path::new("/home/tester");
        assert_eq!(
            expand_home_in(Path::new("~/.local/share/sounds/complete.oga"), Some(home)),
            PathBuf::from("/home/tester/.local/share/sounds/complete.oga")
        );
        assert_eq!(
            expand_home_in(Path::new("~"), Some(home)),
            PathBuf::from("/home/tester")
        );
        // Not a home reference: kept as written.
        for path in [
            "/usr/share/sounds/complete.oga",
            "sounds/complete.oga",
            "~tester/x",
        ] {
            assert_eq!(
                expand_home_in(Path::new(path), Some(home)),
                PathBuf::from(path),
                "path: {path}"
            );
        }
        // No home directory to expand into.
        assert_eq!(
            expand_home_in(Path::new("~/x.oga"), None),
            PathBuf::from("~/x.oga")
        );
    }

    /// A configured file is played on its own; with none configured, the
    /// platform's list is played in the order it declares.
    #[test]
    fn candidates_pin_a_named_file_or_keep_the_platform_order() {
        let defaults: &[&str] = &[
            "/usr/share/sounds/ocean/stereo/dialog-error.oga",
            "/usr/share/sounds/freedesktop/stereo/dialog-error.oga",
        ];
        assert_eq!(
            candidates(None, defaults),
            [
                PathBuf::from("/usr/share/sounds/ocean/stereo/dialog-error.oga"),
                PathBuf::from("/usr/share/sounds/freedesktop/stereo/dialog-error.oga"),
            ]
        );
        assert_eq!(
            candidates(Some(Path::new("~/sounds/error.oga")), defaults),
            [PathBuf::from("~/sounds/error.oga")],
            "a named file replaces the list, and stays unexpanded"
        );
        assert!(candidates(None, &[]).is_empty());
    }

    #[test]
    fn a_missing_file_plays_nothing() {
        assert_eq!(
            resolve_sound(&[PathBuf::from("/nonexistent/nope.oga")]),
            None
        );
        assert_eq!(
            resolve_sound(&[PathBuf::from("~/.no-such-dir/nope.oga")]),
            None
        );
        assert_eq!(resolve_sound(&[]), None);
        assert_eq!(
            resolve_sound(&[
                PathBuf::from("/nonexistent/ocean/dialog-error.oga"),
                PathBuf::from("/nonexistent/freedesktop/dialog-error.oga"),
            ]),
            None,
            "a list of missing candidates is as silent as a single one"
        );
    }

    /// The list is a preference order: the host need have only one of the files
    /// for a sound to play.
    #[test]
    fn the_first_existing_candidate_wins() {
        let dir =
            std::env::temp_dir().join(format!("shuvarie-notify-first-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let freedesktop = dir.join("freedesktop-dialog-error.oga");
        std::fs::write(&freedesktop, b"not really audio").unwrap();
        let ocean = dir.join("ocean-dialog-error.oga");

        assert_eq!(
            resolve_sound(&[ocean.clone(), freedesktop.clone()]),
            Some(freedesktop.clone()),
            "KDE's theme is absent, so the FreeDesktop sound plays"
        );
        std::fs::write(&ocean, b"not really audio").unwrap();
        assert_eq!(
            resolve_sound(&[ocean.clone(), freedesktop.clone()]),
            Some(ocean),
            "with both installed the preferred one plays"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_file_resolves_to_its_expanded_path() {
        let dir = std::env::temp_dir().join(format!("shuvarie-notify-{}", std::process::id()));
        let sounds = dir.join("sounds");
        std::fs::create_dir_all(&sounds).unwrap();
        let path = sounds.join("complete.oga");
        std::fs::write(&path, b"not really audio").unwrap();

        assert_eq!(
            resolve_sound(std::slice::from_ref(&path)),
            Some(path.clone())
        );
        assert_eq!(
            expand_home_in(Path::new("~/sounds/complete.oga"), Some(&dir)),
            path
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The path is appended for the ordinary players, and embedded *instead of*
    /// appended for an entry that puts it inside one of its own arguments.
    #[test]
    fn player_args_embed_or_append_the_path() {
        let plain = Player {
            program: "paplay",
            args: &["--volume", "1"],
        };
        assert_eq!(
            player_args(&plain, Path::new("/sounds/complete.oga")),
            ["--volume", "1", "/sounds/complete.oga"]
        );

        let inline = Player {
            program: "powershell",
            args: &["-Command", "play '{file}'"],
        };
        assert_eq!(
            player_args(&inline, Path::new(r"C:\Windows\Media\notify.wav")),
            ["-Command", r"play 'C:\Windows\Media\notify.wav'"]
        );
    }

    /// An apostrophe in the path would otherwise close the PowerShell string
    /// literal early, so it is doubled.
    #[test]
    fn an_embedded_path_escapes_apostrophes() {
        let inline = Player {
            program: "powershell",
            args: &[
                "-Command",
                "(New-Object Media.SoundPlayer '{file}').PlaySync()",
            ],
        };
        assert_eq!(
            player_args(&inline, Path::new("/Users/o'brien/x.wav")),
            [
                "-Command",
                "(New-Object Media.SoundPlayer '/Users/o''brien/x.wav').PlaySync()",
            ]
        );
    }

    /// Every platform whose default sound is set (see
    /// `shuvarie_config::DEFAULT_NOTIFY_SOUNDS`) needs a player to play it with,
    /// so an empty list here would make that default silently inert.
    #[cfg(any(unix, windows))]
    #[test]
    fn a_platform_with_a_default_sound_has_a_player() {
        assert!(!PLAYERS.is_empty());
    }
}
