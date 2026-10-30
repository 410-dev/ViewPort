package com.viewport.app

import android.Manifest
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Color
import android.graphics.Matrix
import android.media.ExifInterface
import android.media.MediaCodecList
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.storage.StorageManager
import android.provider.DocumentsContract
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.provider.Settings
import android.view.View
import androidx.activity.result.ActivityResult
import androidx.core.content.ContextCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.media3.common.MediaItem
import androidx.media3.common.MimeTypes
import androidx.media3.common.audio.SonicAudioProcessor
import androidx.media3.common.util.UnstableApi
import androidx.media3.effect.Crop
import androidx.media3.effect.ScaleAndRotateTransformation
import androidx.media3.effect.SpeedChangeEffect
import androidx.media3.transformer.Composition
import androidx.media3.transformer.EditedMediaItem
import androidx.media3.transformer.Effects
import androidx.media3.transformer.ExportException
import androidx.media3.transformer.ExportResult
import androidx.media3.transformer.ProgressHolder
import androidx.media3.transformer.Transformer
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.util.concurrent.Executors

@InvokeArg
class PathArgs {
  lateinit var path: String
}

@InvokeArg
class FrameArgs {
  lateinit var path: String
  var atMs: Long = 0
  var maxEdge: Int = 320
  var quality: Int = 85
  /** The frame at that very time (not the keyframe before it); with maxEdge 0, unscaled. */
  var exact: Boolean = false
}

/** A video edit (`vp_core::edit::EditSpec`, flattened by `platform.rs`). */
@InvokeArg
class EditArgs {
  lateinit var input: String
  lateinit var output: String
  /** Clockwise. */
  var rotateDeg: Double = 0.0
  /** Kept part in fractions of the rotated frame's bounding box; cropW 0 keeps it all. */
  var cropX: Double = 0.0
  var cropY: Double = 0.0
  var cropW: Double = 0.0
  var cropH: Double = 0.0
  /** Kept span in ms; endMs < 0 keeps the rest. */
  var startMs: Long = 0
  var endMs: Long = -1
  var speed: Double = 1.0
}

@InvokeArg
class SystemPickArgs {
  var mode: String = "folder"
}

@InvokeArg
class FullscreenArgs {
  var on: Boolean = false
}

@InvokeArg
class SystemBarsArgs {
  var dark: Boolean = true
  var background: String = "#12141a"
}

/**
 * OS services the Rust side cannot reach on its own (see src-tauri/src/platform.rs):
 * all-files storage access, hiding the system bars, and video probing / frame grabs
 * through the platform media framework, which stands in for FFmpeg on Android.
 */
@TauriPlugin(
  permissions = [
    Permission(
      strings = [Manifest.permission.READ_EXTERNAL_STORAGE, Manifest.permission.WRITE_EXTERNAL_STORAGE],
      alias = "storage"
    ),
    Permission(strings = [Manifest.permission.READ_MEDIA_IMAGES, Manifest.permission.READ_MEDIA_VIDEO], alias = "media")
  ]
)
class ViewPortPlugin(private val activity: Activity) : Plugin(activity) {
  // Media work runs off the main thread so thumbnails never stall the UI.
  private val pool = Executors.newFixedThreadPool(2)

  private fun storageGranted(): Boolean =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      Environment.isExternalStorageManager()
    } else {
      ContextCompat.checkSelfPermission(activity, Manifest.permission.READ_EXTERNAL_STORAGE) ==
        PackageManager.PERMISSION_GRANTED
    }

  /** Photos and videos only (Android 13+ media permissions, or the legacy storage permission). */
  private fun mediaGranted(): Boolean {
    val perms = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
      listOf(Manifest.permission.READ_MEDIA_IMAGES, Manifest.permission.READ_MEDIA_VIDEO)
    } else {
      listOf(Manifest.permission.READ_EXTERNAL_STORAGE)
    }
    return perms.all { ContextCompat.checkSelfPermission(activity, it) == PackageManager.PERMISSION_GRANTED }
  }

  private fun resolveGranted(invoke: Invoke) {
    invoke.resolve(JSObject().put("granted", storageGranted()).put("media", mediaGranted()))
  }

  @Command
  fun storageStatus(invoke: Invoke) = resolveGranted(invoke)

  /**
   * All-files access first. Where that screen is missing or offers no switch (Samsung Secure
   * Folder and other work profiles), falls back to the photo and video permissions, which let
   * the app read media by path but not other files.
   */
  @Command
  fun requestStorage(invoke: Invoke) {
    if (storageGranted()) return resolveGranted(invoke)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      val own = Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, Uri.parse("package:${activity.packageName}"))
      for (intent in listOf(own, Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION))) {
        try {
          startActivityForResult(invoke, intent, "storageResult")
          return
        } catch (_: ActivityNotFoundException) {
        } catch (_: SecurityException) {
        }
      }
      requestMedia(invoke)
    } else {
      requestPermissionForAlias("storage", invoke, "storagePermissionResult")
    }
  }

  private fun requestMedia(invoke: Invoke) {
    when {
      mediaGranted() -> resolveGranted(invoke)
      Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU -> requestPermissionForAlias("media", invoke, "storagePermissionResult")
      else -> requestPermissionForAlias("storage", invoke, "storagePermissionResult")
    }
  }

  @ActivityCallback
  fun storageResult(invoke: Invoke, @Suppress("UNUSED_PARAMETER") result: ActivityResult) {
    if (storageGranted()) resolveGranted(invoke) else requestMedia(invoke)
  }

  @PermissionCallback
  fun storagePermissionResult(invoke: Invoke) = resolveGranted(invoke)

  /**
   * Where the in-app file browser starts. The primary volume comes from the OS because it is
   * per user (`/storage/emulated/150` in Samsung Secure Folder, not `/0`). The app's own folder
   * on shared storage is always readable and writable, with or without storage access.
   */
  @Command
  fun storageRoots(invoke: Invoke) {
    val roots = JSArray()
    val seen = mutableSetOf<String>()
    fun add(kind: String, dir: File?) {
      val path = dir?.absolutePath ?: return
      if (!seen.add(path)) return
      roots.put(JSObject().put("kind", kind).put("name", if (kind == "external") dir.name else "").put("path", path))
    }
    add("internal", Environment.getExternalStorageDirectory())
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      activity.getSystemService(StorageManager::class.java)?.storageVolumes?.forEach { v ->
        if (!v.isPrimary && v.state == Environment.MEDIA_MOUNTED) add("external", v.directory)
      }
    } else {
      // Removable volumes: their app folder is <volume>/Android/data/<package>/files.
      activity.getExternalFilesDirs(null).drop(1).filterNotNull().forEach { add("external", File(it.absolutePath.substringBefore("/Android/data/"))) }
    }
    add("app", activity.getExternalFilesDir(null)?.also { it.mkdirs() })
    invoke.resolve(JSObject().put("roots", roots))
  }

  private var pickMode = "folder"

  /**
   * The system document picker, a last resort when the app cannot list a folder itself.
   * The app reads media by path, so the picked document is turned back into one; a file
   * whose path stays unreadable is copied into the app folder.
   */
  @Command
  fun systemPick(invoke: Invoke) {
    pickMode = invoke.parseArgs(SystemPickArgs::class.java).mode
    val intent = if (pickMode == "file") {
      Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("*/*")
    } else {
      Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
    }
    try {
      startActivityForResult(invoke, intent, "systemPickResult")
    } catch (e: ActivityNotFoundException) {
      invoke.reject("no system picker", "unavailable")
    }
  }

  @ActivityCallback
  fun systemPickResult(invoke: Invoke, result: ActivityResult) {
    val uri = result.data?.data
    if (result.resultCode != Activity.RESULT_OK || uri == null) return invoke.resolve(JSObject().put("cancelled", true))
    val tree = pickMode != "file"
    val path = documentPath(uri, tree)
    if (tree) {
      val readable = path != null && File(path).list() != null
      return invoke.resolve(JSObject().put("path", path).put("readable", readable))
    }
    if (path != null && File(path).canRead()) return invoke.resolve(JSObject().put("path", path).put("readable", true))
    pool.execute {
      try {
        val dir = File(activity.getExternalFilesDir(null) ?: activity.filesDir, "picked").apply { mkdirs() }
        val out = File(dir, displayName(uri))
        activity.contentResolver.openInputStream(uri)!!.use { input -> out.outputStream().use { input.copyTo(it, 1 shl 20) } }
        invoke.resolve(JSObject().put("path", out.absolutePath).put("readable", true).put("copied", true))
      } catch (e: Exception) {
        invoke.reject(e.message ?: "copy failed")
      }
    }
  }

  private fun displayName(uri: Uri): String {
    val name = try {
      activity.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
        if (it.moveToFirst()) it.getString(0) else null
      }
    } catch (_: Exception) {
      null
    }
    return (name ?: "picked").replace(Regex("[/\\\\]"), "_")
  }

  /** The file system path behind a document from the external storage or downloads provider. */
  private fun documentPath(uri: Uri, tree: Boolean): String? {
    val id = try {
      if (tree) DocumentsContract.getTreeDocumentId(uri) else DocumentsContract.getDocumentId(uri)
    } catch (_: Exception) {
      return null
    }
    return when (uri.authority) {
      "com.android.externalstorage.documents" -> {
        val volume = id.substringBefore(':')
        val rel = id.substringAfter(':', "")
        val base = when (volume) {
          "primary" -> Environment.getExternalStorageDirectory()
          "home" -> Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOCUMENTS)
          else -> File("/storage/$volume")
        }
        if (rel.isEmpty()) base.absolutePath else File(base, rel).absolutePath
      }
      "com.android.providers.downloads.documents" -> id.removePrefix("raw:").takeIf { it.startsWith("/") }
      else -> null
    }
  }

  @Command
  fun setFullscreen(invoke: Invoke) {
    val args = invoke.parseArgs(FullscreenArgs::class.java)
    activity.runOnUiThread {
      val controller = WindowCompat.getInsetsController(activity.window, activity.window.decorView)
      if (args.on) {
        controller.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        controller.hide(WindowInsetsCompat.Type.systemBars())
      } else {
        controller.show(WindowInsetsCompat.Type.systemBars())
      }
      invoke.resolve()
    }
  }

  /** Colors the area behind the system bars like the page and picks readable bar icons. */
  @Command
  fun systemBars(invoke: Invoke) {
    val args = invoke.parseArgs(SystemBarsArgs::class.java)
    activity.runOnUiThread {
      try {
        activity.findViewById<View>(android.R.id.content).setBackgroundColor(Color.parseColor(args.background))
      } catch (_: IllegalArgumentException) {
      }
      val controller = WindowCompat.getInsetsController(activity.window, activity.window.decorView)
      controller.isAppearanceLightStatusBars = !args.dark
      controller.isAppearanceLightNavigationBars = !args.dark
      invoke.resolve()
    }
  }

  /**
   * When the media store first saw each file under `path` (DATE_ADDED, seconds): the
   * closest Android has to "when this file arrived". Paths are absolute.
   */
  @Command
  fun mediaAdded(invoke: Invoke) {
    val args = invoke.parseArgs(PathArgs::class.java)
    pool.execute {
      try {
        val root = args.path.trimEnd('/')
        val like = root.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_") + "/%"
        val paths = JSArray()
        val times = JSArray()
        activity.contentResolver.query(
          MediaStore.Files.getContentUri(MediaStore.VOLUME_EXTERNAL),
          arrayOf(MediaStore.MediaColumns.DATA, MediaStore.MediaColumns.DATE_ADDED),
          "${MediaStore.MediaColumns.DATA} LIKE ? ESCAPE '\\'",
          arrayOf(like),
          null,
        )?.use { c ->
          while (c.moveToNext()) {
            val path = c.getString(0) ?: continue
            if (c.isNull(1)) continue
            paths.put(path)
            times.put(c.getLong(1))
          }
        }
        invoke.resolve(JSObject().put("paths", paths).put("times", times))
      } catch (e: Exception) {
        invoke.reject(e.message ?: "media store query failed")
      }
    }
  }

  @Command
  fun probe(invoke: Invoke) {
    val args = invoke.parseArgs(PathArgs::class.java)
    pool.execute {
      try {
        invoke.resolve(probeFile(args.path))
      } catch (e: Exception) {
        invoke.reject(e.message ?: "probe failed")
      }
    }
  }

  @Command
  fun frame(invoke: Invoke) {
    val args = invoke.parseArgs(FrameArgs::class.java)
    pool.execute {
      val r = MediaMetadataRetriever()
      try {
        r.setDataSource(args.path)
        val bitmap = if (args.exact && args.maxEdge <= 0) {
          r.getFrameAtTime(args.atMs * 1000, MediaMetadataRetriever.OPTION_CLOSEST) ?: throw Exception("no frame produced")
        } else {
          val edge = args.maxEdge.coerceAtLeast(16)
          grab(r, args.atMs * 1000, edge) ?: grab(r, 0, edge) ?: throw Exception("no frame produced")
        }
        val out = File.createTempFile("vpframe", ".jpg", activity.cacheDir)
        out.outputStream().use { bitmap.compress(Bitmap.CompressFormat.JPEG, args.quality.coerceIn(1, 100), it) }
        bitmap.recycle()
        invoke.resolve(JSObject().put("path", out.absolutePath))
      } catch (e: Exception) {
        invoke.reject(e.message ?: "frame failed")
      } finally {
        r.release()
      }
    }
  }

  /**
   * A copy of a (huge) image scaled to fit `maxEdge`, EXIF orientation applied. The decoder
   * subsamples while decoding (inSampleSize), so a 200-megapixel JPEG never exists in memory
   * at full size.
   */
  @Command
  fun scaledImage(invoke: Invoke) {
    val args = invoke.parseArgs(FrameArgs::class.java)
    pool.execute {
      try {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(args.path, bounds)
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) throw Exception("not a decodable image")
        val edge = args.maxEdge.coerceAtLeast(16)
        // The largest power-of-two subsampling that still decodes to (nearly) the target size;
        // a few percent short is invisible and decodes several times faster.
        var sample = 1
        while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= edge * 0.95) sample *= 2
        val opts = BitmapFactory.Options().apply { inSampleSize = sample }
        var bmp = BitmapFactory.decodeFile(args.path, opts) ?: throw Exception("decode failed")
        val scale = edge.toFloat() / maxOf(bmp.width, bmp.height)
        val matrix = orientation(args.path)
        if (scale < 1f) matrix.preScale(scale, scale)
        if (!matrix.isIdentity) {
          val t = Bitmap.createBitmap(bmp, 0, 0, bmp.width, bmp.height, matrix, true)
          if (t !== bmp) bmp.recycle()
          bmp = t
        }
        val png = bmp.hasAlpha()
        val out = File.createTempFile("vpimage", if (png) ".png" else ".jpg", activity.cacheDir)
        out.outputStream().use {
          bmp.compress(if (png) Bitmap.CompressFormat.PNG else Bitmap.CompressFormat.JPEG, args.quality.coerceIn(1, 100), it)
        }
        val result = JSObject().put("path", out.absolutePath).put("width", bmp.width).put("height", bmp.height).put("png", png)
        bmp.recycle()
        invoke.resolve(result)
      } catch (e: Throwable) {
        // OutOfMemoryError included: the caller falls back to the original.
        invoke.reject(e.message ?: "scale failed")
      }
    }
  }

  /** The EXIF orientation of an image file as a transform. */
  private fun orientation(path: String): Matrix {
    val m = Matrix()
    val o = try {
      ExifInterface(path).getAttributeInt(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL)
    } catch (_: Exception) {
      ExifInterface.ORIENTATION_NORMAL
    }
    when (o) {
      ExifInterface.ORIENTATION_FLIP_HORIZONTAL -> m.setScale(-1f, 1f)
      ExifInterface.ORIENTATION_ROTATE_180 -> m.setRotate(180f)
      ExifInterface.ORIENTATION_FLIP_VERTICAL -> m.setScale(1f, -1f)
      ExifInterface.ORIENTATION_TRANSPOSE -> { m.setRotate(90f); m.postScale(-1f, 1f) }
      ExifInterface.ORIENTATION_ROTATE_90 -> m.setRotate(90f)
      ExifInterface.ORIENTATION_TRANSVERSE -> { m.setRotate(-90f); m.postScale(-1f, 1f) }
      ExifInterface.ORIENTATION_ROTATE_270 -> m.setRotate(-90f)
    }
    return m
  }

  private fun grab(r: MediaMetadataRetriever, atUs: Long, edge: Int): Bitmap? {
    val option = MediaMetadataRetriever.OPTION_CLOSEST_SYNC
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
      return r.getScaledFrameAtTime(atUs, option, edge, edge)
    }
    val full = r.getFrameAtTime(atUs, option) ?: return null
    val scale = edge.toFloat() / maxOf(full.width, full.height)
    if (scale >= 1f) return full
    val scaled = Bitmap.createScaledBitmap(full, (full.width * scale).toInt().coerceAtLeast(1), (full.height * scale).toInt().coerceAtLeast(1), true)
    full.recycle()
    return scaled
  }

  /** Container, codecs (in FFmpeg's names), size and whether this device can play it. */
  private fun probeFile(path: String): JSObject {
    val r = MediaMetadataRetriever()
    val mime: String?
    val duration: Long?
    var width: Int?
    var height: Int?
    try {
      r.setDataSource(path)
      mime = r.extractMetadata(MediaMetadataRetriever.METADATA_KEY_MIMETYPE)
      duration = r.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull()
      width = r.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_WIDTH)?.toIntOrNull()
      height = r.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_HEIGHT)?.toIntOrNull()
      val rotation = r.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_ROTATION)?.toIntOrNull() ?: 0
      if (rotation % 180 != 0) width = height.also { height = width }
    } finally {
      r.release()
    }

    var video: MediaFormat? = null
    var audio: MediaFormat? = null
    val ex = MediaExtractor()
    try {
      ex.setDataSource(path)
      for (i in 0 until ex.trackCount) {
        val f = ex.getTrackFormat(i)
        val m = f.getString(MediaFormat.KEY_MIME) ?: continue
        if (video == null && m.startsWith("video/")) video = f
        if (audio == null && m.startsWith("audio/")) audio = f
      }
    } catch (_: Exception) {
    } finally {
      ex.release()
    }

    val container = when (mime) {
      "video/mp4", "video/quicktime", "video/3gpp", "audio/mp4" -> "mov,mp4,m4a,3gp,3g2,mj2"
      "video/webm" -> "matroska,webm"
      "video/x-matroska" -> "matroska"
      else -> mime ?: ""
    }
    val containerOk = mime in setOf("video/mp4", "video/quicktime", "video/3gpp", "video/webm")
    val playable = containerOk && video != null && canDecode(video) && (audio == null || canDecode(audio))

    val o = JSObject()
    o.put("container", container)
    duration?.let { o.put("durationMs", it) }
    width?.let { o.put("width", it) }
    height?.let { o.put("height", it) }
    video?.getString(MediaFormat.KEY_MIME)?.let { o.put("videoCodec", codecName(it)) }
    audio?.getString(MediaFormat.KEY_MIME)?.let { o.put("audioCodec", codecName(it)) }
    o.put("playable", playable)
    return o
  }

  private fun canDecode(format: MediaFormat): Boolean = try {
    MediaCodecList(MediaCodecList.REGULAR_CODECS).findDecoderForFormat(format) != null
  } catch (_: Exception) {
    false
  }

  private fun codecName(mime: String): String = when (mime) {
    "video/avc" -> "h264"
    "video/hevc" -> "hevc"
    "video/x-vnd.on2.vp8" -> "vp8"
    "video/x-vnd.on2.vp9" -> "vp9"
    "video/av01" -> "av1"
    "video/mp4v-es" -> "mpeg4"
    "video/3gpp" -> "h263"
    "audio/mp4a-latm" -> "aac"
    "audio/mpeg" -> "mp3"
    "audio/opus" -> "opus"
    "audio/vorbis" -> "vorbis"
    "audio/flac" -> "flac"
    "audio/ac3" -> "ac3"
    "audio/eac3" -> "eac3"
    "audio/3gpp" -> "amr_nb"
    "audio/amr-wb" -> "amr_wb"
    "audio/raw" -> "pcm"
    else -> mime.substringAfter('/')
  }

  // ------------------------------------------------------------------ video editing

  /** The export running (one at a time); only touched on the main thread. */
  private var transformer: Transformer? = null

  /** The `editVideo` call and output of [transformer]: `cancel()` calls no listener, so
   *  `editCancel` settles it. */
  private var editing: Pair<Invoke, String>? = null

  /**
   * Edits a video with Media3 Transformer into an H.264/AAC MP4 at `output`. Resolves when
   * the export ends; `editProgress` and `editCancel` act on it meanwhile.
   */
  @androidx.annotation.OptIn(UnstableApi::class)
  @Command
  fun editVideo(invoke: Invoke) {
    val a = invoke.parseArgs(EditArgs::class.java)
    activity.runOnUiThread {
      try {
        if (transformer != null) return@runOnUiThread invoke.reject("another edit is running")
        val clip = MediaItem.ClippingConfiguration.Builder().setStartPositionMs(a.startMs)
        if (a.endMs >= 0) clip.setEndPositionMs(a.endMs)
        val item = MediaItem.Builder().setUri(Uri.fromFile(File(a.input))).setClippingConfiguration(clip.build()).build()
        val video = mutableListOf<androidx.media3.common.Effect>()
        val audio = mutableListOf<androidx.media3.common.audio.AudioProcessor>()
        if (Math.abs(a.speed - 1.0) > 1e-6) {
          video.add(SpeedChangeEffect(a.speed.toFloat()))
          audio.add(SonicAudioProcessor().apply { setSpeed(a.speed.toFloat()) })
        }
        val turn = ((a.rotateDeg % 360) + 360) % 360
        if (turn > 1e-6) {
          // Media3 turns counter-clockwise and grows the frame to fit the turned picture.
          video.add(ScaleAndRotateTransformation.Builder().setRotationDegrees((-turn).toFloat()).build())
        }
        if (a.cropW > 0 && (a.cropX > 0 || a.cropY > 0 || a.cropW < 1 || a.cropH < 1)) {
          // Normalized device coordinates: -1..1, y up.
          val l = (a.cropX * 2 - 1).toFloat()
          val r = ((a.cropX + a.cropW) * 2 - 1).toFloat()
          val t = (1 - a.cropY * 2).toFloat()
          val b = (1 - (a.cropY + a.cropH) * 2).toFloat()
          video.add(Crop(l, r, b, t))
        }
        val edited = EditedMediaItem.Builder(item).setEffects(Effects(audio, video)).build()
        val t = Transformer.Builder(activity)
          .setVideoMimeType(MimeTypes.VIDEO_H264)
          .setAudioMimeType(MimeTypes.AUDIO_AAC)
          .addListener(object : Transformer.Listener {
            override fun onCompleted(composition: Composition, result: ExportResult) {
              transformer = null
              editing = null
              invoke.resolve()
            }

            override fun onError(composition: Composition, result: ExportResult, exception: ExportException) {
              transformer = null
              editing = null
              File(a.output).delete()
              invoke.reject(exception.message ?: "edit failed")
            }
          })
          .build()
        transformer = t
        editing = invoke to a.output
        t.start(edited, a.output)
      } catch (e: Exception) {
        transformer = null
        editing = null
        invoke.reject(e.message ?: "edit failed")
      }
    }
  }

  @androidx.annotation.OptIn(UnstableApi::class)
  @Command
  fun editProgress(invoke: Invoke) {
    activity.runOnUiThread {
      val holder = ProgressHolder()
      val known = transformer?.getProgress(holder) == Transformer.PROGRESS_STATE_AVAILABLE
      invoke.resolve(JSObject().put("progress", if (known) holder.progress / 100.0 else -1.0))
    }
  }

  @Command
  fun editCancel(invoke: Invoke) {
    activity.runOnUiThread {
      transformer?.cancel()
      transformer = null
      editing?.let { (call, output) ->
        File(output).delete()
        call.reject("cancelled")
      }
      editing = null
      invoke.resolve()
    }
  }
}
