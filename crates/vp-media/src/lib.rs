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
use vp_core::edit::EditSpec;

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
        self.run_stream(args, input, out, probe.duration_ms, encoder, progress, cancel)
    }

    /// Runs FFmpeg writing the result to stdout (progress on stderr) into `out`.
    #[allow(clippy::too_many_arguments)]
    fn run_stream(
        &self,
        args: Vec<String>,
        input: Input,
        out: &mut dyn Write,
        total_ms: Option<i64>,
        encoder: &str,
        progress: &mut dyn FnMut(f64),
        cancel: &AtomicBool,
    ) -> Result<()> {
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
        let total_us = total_ms.unwrap_or(0) as f64 * 1000.0;
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
        self.run_convert(args, input, probe.duration_ms, encoder, progress, cancel)
    }

    fn run_convert(
        &self,
        args: Vec<String>,
        input: Input,
        total_ms: Option<i64>,
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
        let total_us = total_ms.unwrap_or(0) as f64 * 1000.0;
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

/// Container FFmpeg writes an edited video into, from the original's extension; None when
/// it cannot hold H.264/AAC (the edit is then written as MP4).
pub fn edit_container(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => "mp4",
        "mov" => "mov",
        "mkv" => "matroska",
        _ => return None,
    })
}

/// FFmpeg video and audio filters for `spec`. The crop is relative (`iw`/`ih`), so it fits
/// the frame as FFmpeg sees it after applying the file's own rotation; the editor always
/// sends an explicit crop for free rotations.
pub fn edit_filters(spec: &EditSpec) -> (String, String) {
    let mut v = Vec::new();
    if (spec.speed - 1.0).abs() > 1e-9 {
        v.push(format!("setpts=PTS/{}", spec.speed));
    }
    match spec.quarter_turns() {
        Some(1) => v.push("transpose=clock".into()),
        Some(2) => v.push("hflip,vflip".into()),
        Some(3) => v.push("transpose=cclock".into()),
        Some(_) => {}
        None => {
            let a = spec.angle().to_radians();
            v.push(format!("rotate=a={a:.9}:ow=rotw({a:.9}):oh=roth({a:.9}):c=black"));
        }
    }
    if let Some(c) = spec.crop.filter(|c| c.x > 0.0 || c.y > 0.0 || c.w < 1.0 || c.h < 1.0) {
        v.push(format!("crop=w=iw*{:.6}:h=ih*{:.6}:x=iw*{:.6}:y=ih*{:.6}", c.w, c.h, c.x, c.y));
    }
    v.push("scale=trunc(iw/2)*2:trunc(ih/2)*2".into());
    v.push("format=yuv420p".into());
    let mut a = Vec::new();
    let mut s = spec.speed;
    while s > 2.0 {
        a.push("atempo=2".to_string());
        s /= 2.0;
    }
    while s < 0.5 {
        a.push("atempo=0.5".to_string());
        s /= 0.5;
    }
    if (s - 1.0).abs() > 1e-9 {
        a.push(format!("atempo={}", (s * 1e6).round() / 1e6));
    }
    (v.join(","), if a.is_empty() { "anull".into() } else { a.join(",") })
}

impl Ffmpeg {
    fn edit_args(input: &Input, spec: &EditSpec, has_audio: bool, encoder: &str, progress_to: &str) -> Vec<String> {
        let mut args: Vec<String> = ["-v", "error", "-y", "-nostats", "-progress", progress_to].map(String::from).to_vec();
        if let Some(t) = spec.trim {
            args.extend(["-ss".into(), secs(t.start_ms), "-t".into(), secs(t.end_ms - t.start_ms)]);
        }
        let (vf, af) = edit_filters(spec);
        args.extend(["-i".into(), input.arg(), "-map".into(), "0:v:0".into(), "-map".into(), "0:a:0?".into()]);
        args.extend(["-vf".into(), vf, "-c:v".into(), encoder.into()]);
        match encoder {
            "libx264" => args.extend(["-preset", "medium", "-crf", "18"].map(String::from)),
            _ => args.extend(["-b:v", "12M"].map(String::from)),
        }
        if has_audio {
            args.extend(["-af".into(), af, "-c:a".into(), "aac".into(), "-b:a".into(), "192k".into()]);
        }
        // The frames are already turned; no rotation tag may turn them again.
        args.extend(["-map_metadata", "0", "-metadata:s:v:0", "rotate=0"].map(String::from));
        args
    }

    /// Applies `spec` to a video and writes it to `output` in `format` (an FFmpeg muxer
    /// from [`edit_container`]). `duration_ms` is the original's length (for progress).
    /// `hw_encoders` are tried in order before libx264.
    #[allow(clippy::too_many_arguments)]
    pub fn edit(
        &self,
        make_input: &mut dyn FnMut() -> Result<Input>,
        output: &Path,
        format: &str,
        spec: &EditSpec,
        probe: &Probe,
        hw_encoders: &[String],
        progress: &mut dyn FnMut(f64),
        cancel: &AtomicBool,
    ) -> Result<()> {
        let total = probe.duration_ms.map(|d| spec.output_duration_ms(d));
        let mut last_err = None;
        for enc in hw_encoders.iter().map(String::as_str).filter(|e| *e != "libx264").chain(["libx264"]) {
            let input = make_input()?;
            let mut args = Self::edit_args(&input, spec, probe.audio_codec.is_some(), enc, "pipe:1");
            if format != "matroska" {
                args.extend(["-movflags", "+faststart"].map(String::from));
            }
            args.extend(["-f".into(), format.into(), output.display().to_string()]);
            match self.run_convert(args, input, total, enc, progress, cancel) {
                Ok(()) => return Ok(()),
                Err(MediaError::Cancelled) => {
                    let _ = std::fs::remove_file(output);
                    return Err(MediaError::Cancelled);
                }
                Err(e) => last_err = Some(e),
            }
        }
        let _ = std::fs::remove_file(output);
        Err(last_err.unwrap_or_else(|| MediaError::Failed("no encoder".into())))
    }

    /// Like [`Ffmpeg::edit`], but streams into writers made by `open` (fragmented MP4/MOV
    /// or Matroska) and completed by `commit`, so the result never touches the disk
    /// unencrypted (shards).
    #[allow(clippy::too_many_arguments)]
    pub fn edit_to_writer<W: Write>(
        &self,
        make_input: &mut dyn FnMut() -> Result<Input>,
        open: &mut dyn FnMut() -> Result<W>,
        commit: &mut dyn FnMut(W) -> Result<()>,
        format: &str,
        spec: &EditSpec,
        probe: &Probe,
        hw_encoders: &[String],
        progress: &mut dyn FnMut(f64),
        cancel: &AtomicBool,
    ) -> Result<()> {
        let total = probe.duration_ms.map(|d| spec.output_duration_ms(d));
        let mut last_err = None;
        for enc in hw_encoders.iter().map(String::as_str).filter(|e| *e != "libx264").chain(["libx264"]) {
            let input = make_input()?;
            let mut args = Self::edit_args(&input, spec, probe.audio_codec.is_some(), enc, "pipe:2");
            if format != "matroska" {
                args.extend(["-movflags", "frag_keyframe+empty_moov+default_base_moof"].map(String::from));
            }
            args.extend(["-f".into(), format.into(), "pipe:1".into()]);
            let mut out = open()?;
            match self.run_stream(args, input, &mut out, total, enc, progress, cancel) {
                Ok(()) => return commit(out),
                Err(MediaError::Cancelled) => return Err(MediaError::Cancelled),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| MediaError::Failed("no encoder".into())))
    }
}

fn secs(ms: i64) -> String {
    format!("{:.3}", ms.max(0) as f64 / 1000.0)
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

    #[test]
    fn edit_filter_chains() {
        use vp_core::edit::{Crop, EditSpec};
        let spec = EditSpec { rotate_deg: 90.0, speed: 4.0, ..Default::default() };
        let (v, a) = edit_filters(&spec);
        assert!(v.starts_with("setpts=PTS/4,transpose=clock,"), "{v}");
        assert_eq!(a, "atempo=2,atempo=2");
        let spec = EditSpec { rotate_deg: 10.0, crop: Some(Crop { x: 0.1, y: 0.2, w: 0.5, h: 0.5 }), speed: 0.25, ..Default::default() };
        let (v, a) = edit_filters(&spec);
        assert!(v.contains("rotate=a=0.174532925:ow=rotw(") && v.contains("crop=w=iw*0.500000"), "{v}");
        assert_eq!(a, "atempo=0.5,atempo=0.5");
        assert_eq!(edit_filters(&EditSpec::default()).1, "anull");
        assert_eq!(edit_container("MKV"), Some("matroska"));
        assert_eq!(edit_container("webm"), None);
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

        // Edits: turned a quarter, cropped to the left half, the first second at double speed.
        use vp_core::edit::{Crop, EditSpec, Trim};
        let spec = EditSpec {
            rotate_deg: 90.0,
            crop: Some(Crop { x: 0.0, y: 0.0, w: 1.0, h: 0.5 }),
            trim: Some(Trim { start_ms: 500, end_ms: 1500 }),
            speed: 2.0,
            ..Default::default()
        };
        let edited = tmp.join("edited.mkv");
        ff.edit(&mut || Ok(Input::File(src.clone())), &edited, "matroska", &spec, &probe, &[], &mut |_| {}, &AtomicBool::new(false)).unwrap();
        let e = ff.probe(Input::File(edited)).unwrap();
        assert_eq!((e.width, e.height), (Some(240), Some(160)), "{e:?}");
        assert!(e.duration_ms.is_some_and(|d| (400..=700).contains(&d)), "{e:?}");
        let free = EditSpec { rotate_deg: 15.0, crop: Some(Crop { x: 0.2, y: 0.2, w: 0.6, h: 0.6 }), ..Default::default() };
        let mut got = None;
        ff.edit_to_writer(
            &mut || Ok(Input::File(src.clone())),
            &mut || Ok(Vec::<u8>::new()),
            &mut |v| {
                got = Some(v);
                Ok(())
            },
            "mp4",
            &free,
            &probe,
            &[],
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        let streamed = tmp.join("edited-stream.mp4");
        std::fs::write(&streamed, got.expect("committed")).unwrap();
        let e = ff.probe(Input::File(streamed)).unwrap();
        assert!(e.width.is_some_and(|w| w % 2 == 0) && e.video_codec.as_deref() == Some("h264"), "{e:?}");
        let _ = std::fs::remove_dir_all(tmp);
    }
}
