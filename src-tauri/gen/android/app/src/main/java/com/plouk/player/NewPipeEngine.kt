package com.plouk.player

import java.io.File
import java.io.IOException
import java.util.concurrent.TimeUnit
import okhttp3.OkHttpClient
import okhttp3.RequestBody.Companion.toRequestBody
import org.schabi.newpipe.extractor.MediaFormat
import org.schabi.newpipe.extractor.NewPipe
import org.schabi.newpipe.extractor.ServiceList
import org.schabi.newpipe.extractor.downloader.Downloader
import org.schabi.newpipe.extractor.downloader.Request
import org.schabi.newpipe.extractor.downloader.Response
import org.schabi.newpipe.extractor.exceptions.ReCaptchaException
import org.schabi.newpipe.extractor.localization.ContentCountry
import org.schabi.newpipe.extractor.localization.Localization
import org.schabi.newpipe.extractor.services.youtube.linkHandler.YoutubeSearchQueryHandlerFactory
import org.schabi.newpipe.extractor.stream.DeliveryMethod
import org.schabi.newpipe.extractor.stream.StreamInfo
import org.schabi.newpipe.extractor.stream.StreamInfoItem

/**
 * Finds a song on YouTube Music with the NewPipe extractor (the library behind
 * the NewPipe app) and downloads its AAC audio stream. Only the audio is
 * fetched, never YouTube's player, so there are no ads.
 */
object NewPipeEngine {
  const val USER_AGENT = "Mozilla/5.0 (Windows NT 10.0; rv:128.0) Gecko/20100101 Firefox/128.0"
  private const val CHUNK = 4L * 1024 * 1024

  val client: OkHttpClient = OkHttpClient.Builder()
    .connectTimeout(20, TimeUnit.SECONDS)
    .readTimeout(30, TimeUnit.SECONDS)
    .build()

  @Volatile private var ready = false

  @Synchronized
  fun init() {
    if (ready) return
    NewPipe.init(OkDownloader(client), Localization("en", "US"), ContentCountry("US"))
    ready = true
  }

  /** Downloads the best match for the song into [dest] and returns its path. */
  fun fetch(title: String, artist: String, durationMs: Long, dest: String): String {
    init()
    val youtube = ServiceList.YouTube
    val query = "$artist $title"
    val wantSecs = durationMs / 1000.0

    var items = search(query, listOf(YoutubeSearchQueryHandlerFactory.MUSIC_SONGS))
    if (items.isEmpty()) items = search("$artist - $title", emptyList())
    val best = items.take(8).maxByOrNull { score(it, title, artist, wantSecs) }
      ?: throw IOException("Couldn't find “$title” on YouTube")

    val info = StreamInfo.getInfo(youtube, best.url)
    val audio = info.audioStreams
      .filter { it.format == MediaFormat.M4A && it.deliveryMethod == DeliveryMethod.PROGRESSIVE_HTTP && it.isUrl }
      .maxByOrNull { it.averageBitrate }
      ?: throw IOException("No playable audio for “$title”")

    download(audio.content, File(dest))
    return dest
  }

  private fun search(query: String, filters: List<String>): List<StreamInfoItem> {
    val youtube = ServiceList.YouTube
    val handler = youtube.searchQHFactory.fromQuery(query, filters, "")
    val extractor = youtube.getSearchExtractor(handler)
    extractor.fetchPage()
    return extractor.initialPage.items.filterIsInstance<StreamInfoItem>()
  }

  /** Same idea as the desktop matcher: close length, right title, right artist, no live/remix versions. */
  private fun score(item: StreamInfoItem, title: String, artist: String, wantSecs: Double): Double {
    val name = item.name.lowercase()
    val uploader = (item.uploaderName ?: "").lowercase()
    val wantTitle = title.lowercase()
    val coreTitle = wantTitle.substringBefore(" - ").substringBefore(" (")
    var s = 0.0
    s -= if (item.duration > 0 && wantSecs > 0) minOf(kotlin.math.abs(item.duration - wantSecs) * 1.5, 90.0) else 20.0
    if (name.contains(coreTitle)) s += 25
    if (uploader.contains(artist.lowercase()) || name.contains(artist.lowercase())) s += 15
    if (uploader.endsWith(" - topic")) s += 20
    for (bad in listOf("live", "cover", "remix", "sped up", "slowed", "nightcore", "8d", "karaoke", "instrumental", "hour", "loop")) {
      if (name.contains(bad) && !wantTitle.contains(bad)) s -= 40
    }
    return s
  }

  /** Ranged chunks keep YouTube's servers from throttling one long request. */
  private fun download(url: String, out: File) {
    out.parentFile?.mkdirs()
    val part = File(out.path + ".part")
    part.outputStream().use { os ->
      var start = 0L
      var total = -1L
      while (total < 0 || start < total) {
        val req = okhttp3.Request.Builder()
          .url(url)
          .header("User-Agent", USER_AGENT)
          .header("Range", "bytes=$start-${start + CHUNK - 1}")
          .build()
        client.newCall(req).execute().use { resp ->
          if (!resp.isSuccessful) throw IOException("Download failed (HTTP ${resp.code})")
          val body = resp.body ?: throw IOException("Empty download")
          val got = body.byteStream().copyTo(os)
          start += got
          total = if (resp.code == 206) {
            resp.header("Content-Range")?.substringAfter('/')?.toLongOrNull() ?: start
          } else {
            start // the server ignored the range and sent everything
          }
          if (got == 0L) total = start
        }
      }
    }
    if (!part.renameTo(out)) throw IOException("Couldn't save the song")
  }

  private class OkDownloader(private val client: OkHttpClient) : Downloader() {
    override fun execute(request: Request): Response {
      val body = request.dataToSend()?.toRequestBody(null)
      val builder = okhttp3.Request.Builder()
        .url(request.url())
        .method(request.httpMethod(), body)
        .header("User-Agent", USER_AGENT)
      for ((name, values) in request.headers()) {
        builder.removeHeader(name)
        for (value in values) builder.addHeader(name, value)
      }
      client.newCall(builder.build()).execute().use { resp ->
        if (resp.code == 429) throw ReCaptchaException("YouTube is asking for a captcha", request.url())
        return Response(resp.code, resp.message, resp.headers.toMultimap(), resp.body?.string(), resp.request.url.toString())
      }
    }
  }
}
