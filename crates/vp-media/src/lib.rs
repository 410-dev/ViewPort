//! Video pipeline on top of an external FFmpeg installation.
//!
//! FFmpeg is located at runtime (explicit path, next to the app, or on `PATH`), so the
//! app works without it and only video-specific features (frame thumbnails, duration
//! and codec probing, conversion) depend on it.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("ffmpeg failed: {0}")]
    Failed(String),
    #[error("cancelled")]
    Cancelled,
    #[error("unexpected ffprobe output: {0}")]
    Parse(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, MediaError>;

/// Where FFmpeg reads the media from.
pub enum Input {
    /// A local file; FFmpeg can seek in it.
    File(PathBuf),
    /// A sequential stream (e.g. from a network driver), fed to FFmpeg's stdin.
    Stream(Box<dyn Read + Send>),
}

impl Input {
    fn arg(&self) -> String {
        match self {
            Input::File(p) => p.display().to_string(),
            Input::Stream(_) => "pipe:0".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Probe {
    /// FFmpeg's container name, e.g. `mov,mp4,m4a,3gp,3g2,mj2` or `matroska,webm`.
    pub container: String,
    pub duration_ms: Option<i64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
}

impl Probe {
    /// Whether the system WebView is expected to play this file natively. Deliberately
    /// conservative: unknown combinations count as incompatible.
    pub fn webview_compatible(&self, ext: &str) -> bool {
        let ext = ext.to_ascii_lowercase();
        let v = self.video_codec.as_deref().unwrap_or("");
        let a = self.audio_codec.as_deref();
        let audio_ok = |ok: &[&str]| a.is_none_or(|a| ok.contains(&a));
        if self.container.contains("mp4") || self.container.contains("mov") {
            matches!(ext.as_str(), "mp4" | "m4v" | "mov") && matches!(v, "h264" | "av1" | "vp9") && audio_ok(&["aac", "mp3", "opus", "flac"])
        } else if self.container.contains("webm") && ext == "webm" {
            matches!(v, "vp8" | "vp9" | "av1") && audio_ok(&["opus", "vorbis"])
        } else {
            false
        }
    }

    /// How to turn this file into a compatible MP4 with as little work as possible.
    pub fn plan(&self) -> ConvertPlan {
        ConvertPlan {
            copy_video: matches!(self.video_codec.as_deref(), Some("h264" | "av1")),
            copy_audio: matches!(self.audio_codec.as_deref(), None | Some("aac" | "mp3")),
            has_audio: self.audio_codec.is_some(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ConvertPlan {
    pub copy_video: bool,
    pub copy_audio: bool,
    pub has_audio: bool,
}

impl ConvertPlan {
    /// A remux only rewrites the container, which is fast and lossless.
    pub fn is_remux(&self) -> bool {
        self.copy_video && self.copy_audio
    }
}

#[derive(Debug, Clone)]
pub struct Ffmpeg {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

fn command(program: &Path) -> Command {
    let mut c = Command::new(program);
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

impl Ffmpeg {
    /// Finds FFmpeg: `explicit` (a file or its folder) if given, then each of `dirs`,
    /// then `PATH`. ffprobe must sit next to ffmpeg.
    pub fn locate(explicit: &str, dirs: &[PathBuf]) -> Option<Self> {
        let try_dir = |dir: &Path| {
            let ffmpeg = dir.join(exe("ffmpeg"));
            let ffprobe = dir.join(exe("ffprobe"));
            (ffmpeg.is_file() && ffprobe.is_file()).then_some(Self { ffmpeg, ffprobe })
        };
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            let p = PathBuf::from(explicit);
            let dir = if p.is_dir() { p } else { p.parent()?.to_path_buf() };
            return try_dir(&dir);
        }
        let path_dirs = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
        dirs.iter().chain(path_dirs.iter()).find_map(|d| try_dir(d))
    }

    pub fn version(&self) -> Option<String> {
        let out = command(&self.ffmpeg).arg("-version").output().ok()?;
        String::from_utf8_lossy(&out.stdout).lines().next().map(str::to_string)
    }

    /// Spawns `program` with `args`, feeding a stream input on a separate thread.
    fn spawn(&self, program: &Path, args: &[String], input: Input) -> Result<(Child, Option<std::thread::JoinHandle<()>>)> {
        let mut cmd = command(program);
        cmd.args(args);
        if matches!(input, Input::Stream(_)) {
            cmd.stdin(Stdio::piped());
        }
        let mut child = cmd.spawn()?;
        let feeder = match input {
            Input::Stream(mut r) => {
                let mut stdin = child.stdin.take().expect("piped stdin");
                Some(std::thread::spawn(move || {
                    // FFmpeg may stop reading early (e.g. after one frame); a broken pipe is fine.
                    let _ = std::io::copy(&mut r, &mut stdin);
                    let _ = stdin.flush();
                }))
            }
            Input::File(_) => None,
        };
        Ok((child, feeder))
    }

    fn run(&self, program: &Path, args: Vec<String>, input: Input) -> Result<Vec<u8>> {
        let (child, feeder) = self.spawn(program, &args, input)?;
        let out = child.wait_with_output()?;
        if let Some(f) = feeder {
            let _ = f.join();
        }
        if !out.status.success() {
            return Err(MediaError::Failed(last_lines(&out.stderr)));
        }
        Ok(out.stdout)
    }

    pub fn probe(&self, input: Input) -> Result<Probe> {
        let args = vec![
            "-v".into(),
            "error".into(),
            "-print_format".into(),
            "json".into(),
            "-show_format".into(),
            "-show_streams".into(),
            input.arg(),
        ];
        let out = self.run(&self.ffprobe.clone(), args, input)?;
        parse_probe(&out)
    }

    /// Grabs one frame at `at_secs`, scaled to fit `max_edge`, as JPEG.
    /// `hwaccel` is an FFmpeg `-hwaccel` method ("auto", "d3d11va", ...).
    pub fn frame(&self, input: Input, at_secs: f64, max_edge: u32, hwaccel: Option<&str>) -> Result<Vec<u8>> {
        self.frame_q(input, at_secs, max_edge, hwaccel, 4)
    }

    /// The frame at `at_secs` at its own size, as JPEG of `quality` (0..=100): a still
    /// saved from a video.
    pub fn still(&self, input: Input, at_secs: f64, quality: u8) -> Result<Vec<u8>> {
        // FFmpeg's JPEG scale runs from 2 (best) to 31.
        let q = 2 + (100 - quality.min(100) as u32) * 29 / 100;
        self.frame_q(input, at_secs, u32::MAX, None, q)
    }

    fn frame_q(&self, input: Input, at_secs: f64, max_edge: u32, hwaccel: Option<&str>, qscale: u32) -> Result<Vec<u8>> {
        let mut args: Vec<String> = vec!["-v".into(), "error".into()];
        if let Some(m) = hwaccel {
            args.extend(["-hwaccel".into(), m.into()]);
        }
        if matches!(input, Input::File(_)) {
            // Fast keyframe seek; only possible on seekable input.
            args.extend(["-ss".into(), format!("{at_secs:.3}")]);
        }
        args.extend(["-i".into(), input.arg()]);
        if !matches!(input, Input::File(_)) {
            args.extend(["-ss".into(), format!("{at_secs:.3}")]);
        }
        args.extend(["-frames:v".into(), "1".into()]);
        if max_edge != u32::MAX {
            let e = max_edge.max(16);
            args.extend(["-vf".into(), format!("scale='if(gt(iw,ih),min({e},iw),-2)':'if(gt(iw,ih),-2,min({e},ih))'")]);
        }
        args.extend([
            "-q:v".into(),
            qscale.to_string(),
            "-f".into(),
            "image2pipe".into(),
            "-vcodec".into(),
            "mjpeg".into(),
            "pipe:1".into(),
        ]);
        let out = self.run(&self.ffmpeg.clone(), args, input)?;
        if out.is_empty() {
            return Err(MediaError::Failed("no frame produced".into()));
        }
        Ok(out)
    }

    /// Decodes a still image (HEIF/HEIC, AVIF, ...: whatever FFmpeg reads) at full size, as
    /// an uncompressed BMP for the caller to scale. Tiled HEIF grids (phone photos) are
    /// assembled by FFmpeg's own filtergraph, which rules out adding `-vf` here; rotation
    /// and mirroring are applied.
    pub fn image(&self, input: Input) -> Result<Vec<u8>> {
        let args: Vec<String> = vec![
            "-v".into(),
            "error".into(),
            "-i".into(),
            input.arg(),
            "-frames:v".into(),
            "1".into(),
            "-f".into(),
            "image2pipe".into(),
            "-c:v".into(),
            "bmp".into(),
            "pipe:1".into(),
        ];
        let out = self.run(&self.ffmpeg.clone(), args, input)?;
        if out.is_empty() {
            return Err(MediaError::Failed("no image produced".into()));
        }
        Ok(out)
    }

    /// Converts to a high-compatibility MP4 (H.264/AAC, fast start) at `output`.
    /// `progress` receives 0.0..=1.0. `hw_encoders` are tried in order first and libx264
    /// is used if none works. `make_input` is called once per attempt.
    pub fn convert(
        &self,
        make_input: &mut dyn FnMut() -> Result<Input>,
        output: &Path,
        probe: &Probe,
        hw_encoders: &[String],
        progress: &mut dyn FnMut(f64),
        cancel: &AtomicBool,
    ) -> Result<()> {
        let plan = probe.plan();
        let encoders: Vec<&str> = if plan.copy_video {
            vec!["copy"]
        } else {
            hw_encoders.iter().map(String::as_str).filter(|e| *e != "libx264").chain(["libx264"]).collect()
        };
        let tmp = output.with_extension("part.mp4");
        let mut last_err = None;
        for enc in encoders {
            match self.convert_with(make_input()?, &tmp, probe, plan, enc, progress, cancel) {
                Ok(()) => {
                    std::fs::rename(&tmp, output)?;
                    return Ok(());
                }
                Err(MediaError::Cancelled) => {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(MediaError::Cancelled);
                }
                Err(e) => last_err = Some(e),
            }
        }
        let _ = std::fs::remove_file(&tmp);
        Err(last_err.unwrap_or_else(|| MediaError::Failed("no encoder".into())))
    }

    /// Like [`Ffmpeg::convert`], but streams a fragmented MP4 into a writer instead of a
    /// file, so the converted copy never touches the disk unencrypted (shards). `open`
    /// makes a writer per attempt and `commit` completes it; a failed attempt's writer is
    /// dropped.
    #[allow(clippy::too_many_arguments)]
    pub fn convert_to_writer<W: Write>(
        &self,
        make_input: &mut dyn FnMut() -> Result<Input>,
        open: &mut dyn FnMut() -> Result<W>,
        commit: &mut dyn FnMut(W) -> Result<()>,
        probe: &Probe,
        hw_encoders: &[String],
        progress: &mut dyn FnMut(f64),
        cancel: &AtomicBool,
    ) -> Result<()> {
        let plan = probe.plan();
        let encoders: Vec<&str> = if plan.copy_video {
            vec!["copy"]
        } else {
            hw_encoders.iter().map(String::as_str).filter(|e| *e != "libx264").chain(["libx264"]).collect()
        };
        let mut last_err = None;
        for enc in encoders {
            let mut out = open()?;
            match self.stream_with(make_input()?, &mut out, probe, plan, enc, progress, cancel) {
                Ok(()) => return commit(out),
                Err(MediaError::Cancelled) => return Err(MediaError::Cancelled),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| MediaError::Failed("no encoder".into())))
    }

    #[allow(clippy::too_many_arguments)]
    fn stream_with(
        &self,
        input: Input,
        out: &mut dyn Write,
        probe: &Probe,
        plan: ConvertPlan,
        encoder: &str,
        progress: &mut dyn FnMut(f64),
        cancel: &AtomicBool,
    ) -> Result<()> {
        // Progress goes to stderr, since stdout carries the video.
        let mut args = Self::convert_args(&input, plan, encoder, "pipe:2");
        args.extend(["-movflags", "frag_keyframe+empty_moov+default_base_moof", "-f", "mp4", "pipe:1"].map(String::from));
        let (mut child, feeder) = self.spawn(&self.ffmpeg, &args, input)?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (tx, rx) = std::sync::mpsc::channel::<i64>();
        let err_reader = std::thread::spawn(move || {
            let mut errors = Vec::new();
            for line in BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
                if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.parse::<i64>().ok()) {
                    let _ = tx.send(us);
                } else if !line.split_once('=').is_some_and(|(k, _)| !k.is_empty() && !k.contains(' ')) {
                    errors.extend_from_slice(line.as_bytes());
                    errors.push(b'\n');
                }
            }
            errors
        });
        let total_us = probe.duration_ms.unwrap_or(0) as f64 * 1000.0;
        let mut buf = vec![0u8; 256 * 1024];
        let mut write_err = None;
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(MediaError::Cancelled);
            }
            let n = match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            if let Err(e) = out.write_all(&buf[..n]) {
                let _ = child.kill();
                write_err = Some(e);
                break;
            }
            while let Ok(us) = rx.try_recv() {
                if total_us > 0.0 {
                    progress((us as f64 / total_us).clamp(0.0, 1.0));
                }
            }
        }
        let status = child.wait()?;
        if let Some(f) = feeder {
            let _ = f.join();
        }
        let stderr = err_reader.join().unwrap_or_default();
        if let Some(e) = write_err {
            return Err(e.into());
        }
        if !status.success() {
            return Err(MediaError::Failed(format!("{encoder}: {}", last_lines(&stderr))));
        }
        progress(1.0);
        Ok(())
    }

    fn convert_args(input: &Input, plan: ConvertPlan, encoder: &str, progress_to: &str) -> Vec<String> {
        let mut args: Vec<String> =
            vec!["-v".into(), "error".into(), "-y".into(), "-nostats".into(), "-progress".into(), progress_to.into()];
        args.extend(["-i".into(), input.arg(), "-map".into(), "0:v:0".into(), "-map".into(), "0:a:0?".into()]);
        args.extend(["-c:v".into(), encoder.into()]);
        match encoder {
            "copy" => {}
            "libx264" => args.extend(["-preset", "veryfast", "-crf", "20"].map(String::from)),
            _ => args.extend(["-b:v", "8M"].map(String::from)),
        }
        if encoder != "copy" {
            args.extend(["-pix_fmt", "yuv420p"].map(String::from));
        }
        if plan.has_audio {
            if plan.copy_audio {
                args.extend(["-c:a", "copy"].map(String::from));
            } else {
                args.extend(["-c:a", "aac", "-b:a", "192k"].map(String::from));
            }
        }
        args
    }

    #[allow(clippy::too_many_arguments)]
    fn convert_with(
        &self,
        input: Input,
        output: &Path,
        probe: &Probe,
        plan: ConvertPlan,
        encoder: &str,
        progress: &mut dyn FnMut(f64),
        cancel: &AtomicBool,
    ) -> Result<()> {
        let mut args = Self::convert_args(&input, plan, encoder, "pipe:1");
        args.extend(["-movflags", "+faststart", "-f", "mp4"].map(String::from));
        args.push(output.display().to_string());
        self.run_convert(args, input, probe, encoder, progress, cancel)
    }

    fn run_convert(
        &self,
        args: Vec<String>,
        input: Input,
        probe: &Probe,
        encoder: &str,
        progress: &mut dyn FnMut(f64),
        cancel: &AtomicBool,
    ) -> Result<()> {
        let (mut child, feeder) = self.spawn(&self.ffmpeg, &args, input)?;
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let err_reader = std::thread::spawn(move || {
            let mut s = Vec::new();
            let _ = stderr.read_to_end(&mut s);
            s
        });
        let total_us = probe.duration_ms.unwrap_or(0) as f64 * 1000.0;
        for line in BufReader::new(stdout).lines() {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(MediaError::Cancelled);
            }
            let Ok(line) = line else { break };
            if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok())
                && total_us > 0.0 {
                    progress((us / total_us).clamp(0.0, 1.0));
                }
        }
        let status = child.wait()?;
        if let Some(f) = feeder {
            let _ = f.join();
        }
        let stderr = err_reader.join().unwrap_or_default();
        if !status.success() {
            return Err(MediaError::Failed(format!("{encoder}: {}", last_lines(&stderr))));
        }
        progress(1.0);
        Ok(())
    }
}

fn last_lines(stderr: &[u8]) -> String {
    let s = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join(" | ")
}

pub fn parse_probe(json: &[u8]) -> Result<Probe> {
    #[derive(Deserialize)]
    struct Out {
        format: Option<Format>,
        #[serde(default)]
        streams: Vec<Stream>,
    }
    #[derive(Deserialize)]
    struct Format {
        format_name: Option<String>,
        duration: Option<String>,
    }
    #[derive(Deserialize)]
    struct Stream {
        codec_type: Option<String>,
        codec_name: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
        duration: Option<String>,
        #[serde(default)]
        disposition: Disposition,
        #[serde(default)]
        side_data_list: Vec<serde_json::Value>,
    }
    #[derive(Deserialize, Default)]
    struct Disposition {
        #[serde(default)]
        attached_pic: u8,
    }
    let out: Out = serde_json::from_slice(json).map_err(|e| MediaError::Parse(e.to_string()))?;
    let secs = |s: &Option<String>| s.as_deref().and_then(|d| d.parse::<f64>().ok()).filter(|d| d.is_finite() && *d > 0.0);
    let video = out.streams.iter().find(|s| s.codec_type.as_deref() == Some("video") && s.disposition.attached_pic == 0);
    let audio = out.streams.iter().find(|s| s.codec_type.as_deref() == Some("audio"));
    let format = out.format.as_ref();
    let duration = format.and_then(|f| secs(&f.duration)).or_else(|| video.and_then(|v| secs(&v.duration)));
    // Portrait phone videos are stored landscape with a rotation tag.
    let rotated = video.is_some_and(|v| {
        v.side_data_list.iter().any(|d| d.get("rotation").and_then(|r| r.as_i64()).is_some_and(|r| r.abs() == 90))
    });
    let (w, h) = (video.and_then(|v| v.width), video.and_then(|v| v.height));
    Ok(Probe {
        container: format.and_then(|f| f.format_name.clone()).unwrap_or_default(),
        duration_ms: duration.map(|d| (d * 1000.0).round() as i64),
        width: if rotated { h } else { w },
        height: if rotated { w } else { h },
        video_codec: video.and_then(|v| v.codec_name.clone()),
        audio_codec: audio.and_then(|a| a.codec_name.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_probe_json() {
        let json = br#"{"streams":[
            {"codec_type":"video","codec_name":"h264","width":1920,"height":1080,
             "side_data_list":[{"rotation":-90}]},
            {"codec_type":"audio","codec_name":"aac"},
            {"codec_type":"video","codec_name":"mjpeg","disposition":{"attached_pic":1}}],
            "format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"12.345000"}}"#;
        let p = parse_probe(json).unwrap();
        assert_eq!(p.duration_ms, Some(12_345));
        assert_eq!((p.width, p.height), (Some(1080), Some(1920)), "rotation swaps dimensions");
        assert_eq!(p.video_codec.as_deref(), Some("h264"));
        assert!(p.webview_compatible("mp4"));
        assert!(p.plan().is_remux());
    }

    #[test]
    fn compatibility_rules() {
        let p = |c: &str, v: &str, a: Option<&str>| Probe {
            container: c.into(),
            video_codec: Some(v.into()),
            audio_codec: a.map(Into::into),
            ..Default::default()
        };
        assert!(p("matroska,webm", "vp9", Some("opus")).webview_compatible("webm"));
        assert!(!p("matroska,webm", "h264", Some("aac")).webview_compatible("mkv"));
        assert!(!p("mov,mp4", "hevc", Some("aac")).webview_compatible("mp4"));
        assert!(!p("avi", "mpeg4", Some("mp3")).webview_compatible("avi"));
        let plan = p("matroska,webm", "h264", Some("ac3")).plan();
        assert!(plan.copy_video && !plan.copy_audio && !plan.is_remux());
    }

    /// A phone-style HEIC (2x2 grid of 256 px HEVC tiles, rotated) decodes whole.
    /// `testdata/mkheic.py` made the file.
    #[test]
    fn real_ffmpeg_heic_grid() {
        let Ok(dir) = std::env::var("VP_TEST_FFMPEG") else { return };
        let ff = Ffmpeg::locate(&dir, &[]).expect("ffmpeg in VP_TEST_FFMPEG");
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/grid.heic");
        let bmp = ff.image(Input::File(path)).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        let dims = |b: &[u8]| (i32::from_le_bytes(b[18..22].try_into().unwrap()), i32::from_le_bytes(b[22..26].try_into().unwrap()).abs());
        assert_eq!(dims(&bmp), (512, 512), "all four tiles");
    }

    /// Runs against a real FFmpeg when `VP_TEST_FFMPEG` points at its folder.
    #[test]
    fn real_ffmpeg_round_trip() {
        let Ok(dir) = std::env::var("VP_TEST_FFMPEG") else { return };
        let ff = Ffmpeg::locate(&dir, &[]).expect("ffmpeg in VP_TEST_FFMPEG");
        let tmp = std::env::temp_dir().join(format!("vp-media-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("in.mkv");
        let ok = command(&ff.ffmpeg)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=10", "-t", "2", "-c:v", "mpeg4"])
            .arg(&src)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let probe = ff.probe(Input::File(src.clone())).unwrap();
        assert_eq!((probe.width, probe.height), (Some(320), Some(240)));
        assert!(!probe.webview_compatible("mkv"));
        let jpg = ff.frame(Input::File(src.clone()), 0.5, 100, None).unwrap();
        assert_eq!(&jpg[..2], &[0xFF, 0xD8]);
        let piped = ff.frame(Input::Stream(Box::new(std::fs::File::open(&src).unwrap())), 0.5, 100, None).unwrap();
        assert_eq!(&piped[..2], &[0xFF, 0xD8]);
        let still = ff.still(Input::File(src.clone()), 0.5, 95).unwrap();
        assert_eq!(&still[..2], &[0xFF, 0xD8]);
        let out = tmp.join("out.mp4");
        let mut last = 0.0;
        ff.convert(&mut || Ok(Input::File(src.clone())), &out, &probe, &[], &mut |p| last = p, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(last, 1.0);
        let converted = ff.probe(Input::File(out)).unwrap();
        assert!(converted.webview_compatible("mp4"), "{converted:?}");

        // Streamed into a writer (fragmented MP4) instead of a file.
        let mut got = None;
        ff.convert_to_writer(
            &mut || Ok(Input::File(src.clone())),
            &mut || Ok(Vec::<u8>::new()),
            &mut |v| {
                got = Some(v);
                Ok(())
            },
            &probe,
            &[],
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        let streamed = tmp.join("streamed.mp4");
        std::fs::write(&streamed, got.expect("committed")).unwrap();
        let p = ff.probe(Input::File(streamed)).unwrap();
        assert!(p.webview_compatible("mp4") && p.duration_ms.is_some_and(|d| d > 0), "{p:?}");
        let _ = std::fs::remove_dir_all(tmp);
    }
}
