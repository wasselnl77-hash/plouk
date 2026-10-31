package com.plouk.player

import android.Manifest
import android.app.Activity
import android.content.pm.PackageManager
import android.os.Build
import android.webkit.WebView
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.util.concurrent.Executors

@InvokeArg
class FetchArgs {
  var title: String = ""
  var artist: String = ""
  var durationMs: Long = 0
  var dest: String = ""
}

@InvokeArg
class ChannelArgs {
  lateinit var channel: Channel
}

@InvokeArg
class MetadataArgs {
  var title: String = ""
  var artist: String = ""
  var album: String = ""
  var cover: String? = null
  var durationMs: Long = 0
}

@InvokeArg
class PlaybackArgs {
  var playing: Boolean = false
  var positionMs: Long = 0
}

/** The bridge the Rust side talks to (see src/android.rs). */
@TauriPlugin
class PloukPlugin(private val activity: Activity) : Plugin(activity) {
  private val io = Executors.newFixedThreadPool(3)
  private var askedForNotifications = false

  companion object {
    /** Link shared before the page was ready (cold start). */
    @Volatile var pendingShare: String? = null
    @Volatile private var events: Channel? = null

    private fun emit(kind: String, fill: JSObject.() -> Unit) {
      val channel = events ?: return
      val payload = JSObject()
      payload.put("kind", kind)
      payload.fill()
      channel.send(payload)
    }

    fun mediaKey(action: String, value: Double? = null) = emit("media-key") {
      put("action", action)
      if (value != null) put("value", value)
    }

    /** A link shared while Plouk is already running. */
    fun shared(text: String) = emit("shared") { put("text", text) }
  }

  override fun load(webView: WebView) {
    io.execute { runCatching { NewPipeEngine.init() } }
  }

  @Command
  fun setEventChannel(invoke: Invoke) {
    events = invoke.parseArgs(ChannelArgs::class.java).channel
    invoke.resolve()
  }

  @Command
  fun takeShared(invoke: Invoke) {
    val result = JSObject()
    result.put("text", pendingShare)
    pendingShare = null
    invoke.resolve(result)
  }

  @Command
  fun fetch(invoke: Invoke) {
    val args = invoke.parseArgs(FetchArgs::class.java)
    io.execute {
      try {
        val path = NewPipeEngine.fetch(args.title, args.artist, args.durationMs, args.dest)
        val result = JSObject()
        result.put("path", path)
        invoke.resolve(result)
      } catch (e: Exception) {
        invoke.reject(e.message ?: "Couldn't find “${args.title}”")
      }
    }
  }

  @Command
  fun setMetadata(invoke: Invoke) {
    val args = invoke.parseArgs(MetadataArgs::class.java)
    PlaybackService.title = args.title
    PlaybackService.artist = args.artist
    PlaybackService.album = args.album
    PlaybackService.durationMs = args.durationMs
    PlaybackService.setCover(args.cover)
    PlaybackService.update(activity)
    invoke.resolve()
  }

  @Command
  fun setPlayback(invoke: Invoke) {
    val args = invoke.parseArgs(PlaybackArgs::class.java)
    PlaybackService.playing = args.playing
    PlaybackService.positionMs = args.positionMs
    if (args.playing) askForNotifications()
    PlaybackService.update(activity)
    invoke.resolve()
  }

  /** Android 13+ needs permission before the playback notification can show. */
  private fun askForNotifications() {
    if (askedForNotifications || Build.VERSION.SDK_INT < 33) return
    askedForNotifications = true
    if (activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
      activity.runOnUiThread { activity.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 7) }
    }
  }
}
