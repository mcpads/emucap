use std::collections::BTreeSet;

pub const INPUT_MOVIE_FORMAT: &str = "frame-full-state-1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovieFrame {
    pub frame: u64,
    pub buttons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Movie {
    pub frames: Vec<MovieFrame>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalMovie {
    pub movie: Movie,
    pub bytes: Vec<u8>,
}

/// Parses `<frame>:<button>,<button>` rows. Each row is a full pressed-button set. Blank rows are
/// ignored, button names are trimmed and lowercased, and frames are sorted for the existing
/// regression runner.
pub fn parse_movie(text: &str) -> Result<Movie, String> {
    let mut frames = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let (frame, buttons) = line
            .split_once(':')
            .ok_or_else(|| format!("line {}: missing ':'", index + 1))?;
        let frame: u64 = frame
            .trim()
            .parse()
            .map_err(|_| format!("line {}: frame is not an integer", index + 1))?;
        let buttons = buttons
            .split(',')
            .map(|button| button.trim().to_ascii_lowercase())
            .filter(|button| !button.is_empty())
            .collect();
        frames.push(MovieFrame { frame, buttons });
    }
    frames.sort_by_key(|frame| frame.frame);
    Ok(Movie { frames })
}

pub fn canonical_recording_movie(
    text: &str,
    frames: u64,
    max_buttons_per_frame: u64,
) -> Result<CanonicalMovie, String> {
    let mut movie = parse_movie(text)?;
    if frames == 0 {
        return Err("recording movie frame count must be positive".into());
    }
    if movie.frames.len() != usize::try_from(frames).unwrap_or(usize::MAX) {
        return Err(format!(
            "recording movie must contain exactly {frames} nonblank rows"
        ));
    }

    let mut canonical = String::new();
    for (expected, frame) in movie.frames.iter_mut().enumerate() {
        let expected = u64::try_from(expected).map_err(|_| "recording movie is too large")?;
        if frame.frame != expected {
            return Err(format!(
                "recording movie offsets must be dense from zero: expected {expected}, got {}",
                frame.frame
            ));
        }
        if u64::try_from(frame.buttons.len()).unwrap_or(u64::MAX) > max_buttons_per_frame {
            return Err(format!(
                "recording movie frame {expected} exceeds {max_buttons_per_frame} buttons"
            ));
        }
        let mut seen = BTreeSet::new();
        for button in &frame.buttons {
            if button.is_empty()
                || !button.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'+' | b'-')
                })
            {
                return Err(format!(
                    "recording movie frame {expected} contains invalid button {button:?}"
                ));
            }
            if !seen.insert(button.clone()) {
                return Err(format!(
                    "recording movie frame {expected} contains duplicate button {button}"
                ));
            }
        }
        frame.buttons = seen.into_iter().collect();
        canonical.push_str(&expected.to_string());
        canonical.push(':');
        canonical.push_str(&frame.buttons.join(","));
        canonical.push('\n');
    }

    Ok(CanonicalMovie {
        movie,
        bytes: canonical.into_bytes(),
    })
}
