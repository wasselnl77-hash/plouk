package com.plouk.player

import android.content.Intent
import android.graphics.Color
import android.os.Bundle
import android.view.View
import androidx.activity.OnBackPressedCallback
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge(
      statusBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
      navigationBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
    )
    super.onCreate(savedInstanceState)
    window.decorView.setBackgroundColor(Color.rgb(5, 5, 7))

    // Keep the interface clear of the status bar, navigation bar and camera cutout.
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
      insets
    }

    // Back sends Plouk to the background instead of stopping the music.
    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        moveTaskToBack(true)
      }
    })

    sharedText(intent)?.let { PloukPlugin.pendingShare = it }
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    sharedText(intent)?.let { PloukPlugin.shared(it) }
  }

  /** "Share → Plouk" from the Spotify app arrives as plain text with a link in it. */
  private fun sharedText(intent: Intent?): String? =
    if (intent?.action == Intent.ACTION_SEND && intent.type?.startsWith("text/") == true) {
      intent.getStringExtra(Intent.EXTRA_TEXT)
    } else {
      null
    }
}
