package com.plouk.player

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.os.Build
import android.os.IBinder
import android.support.v4.media.MediaMetadataCompat
import android.support.v4.media.session.MediaSessionCompat
import android.support.v4.media.session.PlaybackStateCompat
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import androidx.media.session.MediaButtonReceiver
import kotlin.concurrent.thread

/**
 * Keeps Plouk playing with the screen off and puts the song in the
 * notification, on the lock screen and under the headset buttons. The audio
 * itself comes from the Rust engine in the same process; this service just
 * keeps that process alive and relays the controls.
 */
class PlaybackService : Service() {
  companion object {
    private const val CHANNEL_ID = "plouk.playback"
    private const val NOTIFICATION_ID = 7

    @Volatile var title = ""
    @Volatile var artist = ""
    @Volatile var album = ""
    @Volatile var durationMs = 0L
    @Volatile var positionMs = 0L
    @Volatile var playing = false
    @Volatile private var art: Bitmap? = null
    @Volatile private var artUrl: String? = null
    @Volatile private var instance: PlaybackService? = null

    fun setCover(url: String?) {
      if (url == artUrl) return
      artUrl = url
      art = null
      if (url.isNullOrEmpty()) return
      thread(name = "plouk-cover") {
        try {
          val req = okhttp3.Request.Builder().url(url).build()
          NewPipeEngine.client.newCall(req).execute().use { resp ->
            val bmp = resp.body?.byteStream()?.let(BitmapFactory::decodeStream)
            if (url == artUrl) {
              art = bmp
              instance?.refresh()
            }
          }
        } catch (_: Exception) {}
      }
    }

    /** Starts the service when music starts, then keeps the session in sync. */
    fun update(context: Context) {
      val running = instance
      if (running != null) {
        running.refresh()
      } else if (playing) {
        ContextCompat.startForegroundService(context, Intent(context, PlaybackService::class.java))
      }
    }
  }

  private lateinit var session: MediaSessionCompat

  override fun onCreate() {
    super.onCreate()
    instance = this
    session = MediaSessionCompat(this, "Plouk").apply {
      setCallback(object : MediaSessionCompat.Callback() {
        override fun onPlay() = PloukPlugin.mediaKey("play")
        override fun onPause() = PloukPlugin.mediaKey("pause")
        override fun onStop() = PloukPlugin.mediaKey("pause")
        override fun onSkipToNext() = PloukPlugin.mediaKey("next")
        override fun onSkipToPrevious() = PloukPlugin.mediaKey("prev")
        override fun onSeekTo(pos: Long) = PloukPlugin.mediaKey("setPos", pos / 1000.0)
      })
      isActive = true
    }
    val manager = getSystemService(NotificationManager::class.java)
    manager.createNotificationChannel(
      NotificationChannel(CHANNEL_ID, "Now playing", NotificationManager.IMPORTANCE_LOW).apply {
        description = "Plouk's playback controls"
        setShowBadge(false)
      }
    )
  }

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    MediaButtonReceiver.handleIntent(session, intent)
    refresh()
    return START_NOT_STICKY
  }

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onDestroy() {
    instance = null
    session.release()
    super.onDestroy()
  }

  fun refresh() {
    session.setMetadata(
      MediaMetadataCompat.Builder()
        .putString(MediaMetadataCompat.METADATA_KEY_TITLE, title)
        .putString(MediaMetadataCompat.METADATA_KEY_ARTIST, artist)
        .putString(MediaMetadataCompat.METADATA_KEY_ALBUM, album)
        .putLong(MediaMetadataCompat.METADATA_KEY_DURATION, durationMs)
        .putBitmap(MediaMetadataCompat.METADATA_KEY_ALBUM_ART, art)
        .build()
    )
    session.setPlaybackState(
      PlaybackStateCompat.Builder()
        .setActions(
          PlaybackStateCompat.ACTION_PLAY or PlaybackStateCompat.ACTION_PAUSE or PlaybackStateCompat.ACTION_PLAY_PAUSE or
            PlaybackStateCompat.ACTION_SKIP_TO_NEXT or PlaybackStateCompat.ACTION_SKIP_TO_PREVIOUS or PlaybackStateCompat.ACTION_SEEK_TO
        )
        .setState(if (playing) PlaybackStateCompat.STATE_PLAYING else PlaybackStateCompat.STATE_PAUSED, positionMs, if (playing) 1f else 0f)
        .build()
    )
    val notification = buildNotification()
    if (Build.VERSION.SDK_INT >= 29) {
      startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
    } else {
      startForeground(NOTIFICATION_ID, notification)
    }
    // While paused the notification can be swiped away and Android may reclaim the service.
    if (!playing) stopForeground(STOP_FOREGROUND_DETACH)
  }

  private fun action(icon: Int, label: String, action: Long) =
    NotificationCompat.Action(icon, label, MediaButtonReceiver.buildMediaButtonPendingIntent(this, action))

  private fun buildNotification(): Notification {
    val open = PendingIntent.getActivity(
      this, 0,
      Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    return NotificationCompat.Builder(this, CHANNEL_ID)
      .setSmallIcon(R.drawable.ic_stat_plouk)
      .setContentTitle(title)
      .setContentText(artist)
      .setSubText(album)
      .setLargeIcon(art)
      .setContentIntent(open)
      .setDeleteIntent(MediaButtonReceiver.buildMediaButtonPendingIntent(this, PlaybackStateCompat.ACTION_STOP))
      .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
      .setOnlyAlertOnce(true)
      .setOngoing(playing)
      .addAction(action(android.R.drawable.ic_media_previous, "Previous", PlaybackStateCompat.ACTION_SKIP_TO_PREVIOUS))
      .addAction(
        if (playing) action(android.R.drawable.ic_media_pause, "Pause", PlaybackStateCompat.ACTION_PAUSE)
        else action(android.R.drawable.ic_media_play, "Play", PlaybackStateCompat.ACTION_PLAY)
      )
      .addAction(action(android.R.drawable.ic_media_next, "Next", PlaybackStateCompat.ACTION_SKIP_TO_NEXT))
      .setStyle(
        androidx.media.app.NotificationCompat.MediaStyle()
          .setMediaSession(session.sessionToken)
          .setShowActionsInCompactView(0, 1, 2)
      )
      .build()
  }
}
