package com.viewport.app

import android.Manifest
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Color
import android.media.MediaCodecList
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.Settings
import android.view.View
import androidx.activity.result.ActivityResult
import androidx.core.content.ContextCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
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
    )
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

  private fun resolveGranted(invoke: Invoke) {
    invoke.resolve(JSObject().put("granted", storageGranted()))
  }

  @Command
  fun storageStatus(invoke: Invoke) = resolveGranted(invoke)

  @Command
  fun requestStorage(invoke: Invoke) {
    if (storageGranted()) return resolveGranted(invoke)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      val own = Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, Uri.parse("package:${activity.packageName}"))
      try {
        startActivityForResult(invoke, own, "storageResult")
      } catch (_: ActivityNotFoundException) {
        startActivityForResult(invoke, Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION), "storageResult")
      }
    } else {
      requestPermissionForAlias("storage", invoke, "storagePermissionResult")
    }
  }

  @ActivityCallback
  fun storageResult(invoke: Invoke, @Suppress("UNUSED_PARAMETER") result: ActivityResult) = resolveGranted(invoke)

  @PermissionCallback
  fun storagePermissionResult(invoke: Invoke) = resolveGranted(invoke)

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
        val edge = args.maxEdge.coerceAtLeast(16)
        val bitmap = grab(r, args.atMs * 1000, edge) ?: grab(r, 0, edge) ?: throw Exception("no frame produced")
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
}
