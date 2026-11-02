package com.plouk.player

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.core.content.FileProvider
import java.io.File
import java.io.IOException
import okhttp3.OkHttpClient
import okhttp3.Request
import org.json.JSONObject

/** Hands the downloaded APK to the system installer (see Updater). */
class UpdateFileProvider : FileProvider()

/**
 * Plouk updating itself on Android: latest.json on the newest GitHub release names the
 * version, the APK next to it gets downloaded, and the system installer takes it from
 * there. Android only installs it over this one if it's signed with the same key.
 */
object Updater {
  private val http = OkHttpClient()

  private fun apk(context: Context) = File(context.cacheDir, "updates/plouk.apk")

  /** Downloads the newer APK if there is one and returns its version, else null. */
  fun check(context: Context, manifest: String, current: String): String? {
    val json = get(manifest) { it.body!!.string() }
    val latest = JSONObject(json).getString("version").removePrefix("v")
    if (!newer(latest, current)) return null

    val file = apk(context)
    file.parentFile?.mkdirs()
    val part = File(file.path + ".part")
    get("https://github.com/wasselnl77-hash/plouk/releases/download/v$latest/plouk-$latest-android.apk") { r ->
      part.outputStream().use { out -> r.body!!.byteStream().copyTo(out) }
    }
    if (!part.renameTo(file)) throw IOException("Couldn't save the update")
    return latest
  }

  fun install(activity: Activity) {
    // Android asks once per app before it may install anything; send people to that switch first.
    if (!activity.packageManager.canRequestPackageInstalls()) {
      activity.startActivity(
        Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${activity.packageName}"))
      )
      return
    }
    val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.updates", apk(activity))
    activity.startActivity(
      Intent(Intent.ACTION_VIEW)
        .setDataAndType(uri, "application/vnd.android.package-archive")
        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
    )
  }

  private fun <T> get(url: String, read: (okhttp3.Response) -> T): T =
    http.newCall(Request.Builder().url(url).build()).execute().use { r ->
      if (!r.isSuccessful) throw IOException("HTTP ${r.code} for $url")
      read(r)
    }

  private fun newer(a: String, b: String): Boolean {
    val x = a.split('.').map { it.toIntOrNull() ?: 0 }
    val y = b.split('.').map { it.toIntOrNull() ?: 0 }
    for (i in 0 until maxOf(x.size, y.size)) {
      val d = x.getOrElse(i) { 0 } - y.getOrElse(i) { 0 }
      if (d != 0) return d > 0
    }
    return false
  }
}
