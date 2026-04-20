package com.companion.awareness

import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.PixelFormat
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.Image
import android.media.ImageReader
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Handler
import android.os.Looper
import android.util.DisplayMetrics
import android.view.WindowManager
import com.google.mlkit.vision.common.InputImage
import com.google.mlkit.vision.text.TextRecognition
import com.google.mlkit.vision.text.latin.TextRecognizerOptions
import java.util.concurrent.atomic.AtomicReference

/**
 * Captures the screen via MediaProjection, runs ML Kit text recognition
 * on each frame, and caches the latest OCR text for the service tick loop.
 */
class ScreenCapture(
    private val ctx: Context,
    private val resultCode: Int,
    private val data: Intent,
) {
    private var projection: MediaProjection? = null
    private var virtualDisplay: VirtualDisplay? = null
    private var reader: ImageReader? = null
    private val recognizer = TextRecognition.getClient(TextRecognizerOptions.DEFAULT_OPTIONS)
    private val latest = AtomicReference("")

    fun start() {
        val mpm = ctx.getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        val proj = mpm.getMediaProjection(resultCode, data)
        projection = proj

        // Android 14 (API 34) REQUIRES a registered callback before
        // `createVirtualDisplay` — without it `IllegalStateException:
        // Must register a callback before starting capture` kills the
        // foreground service and the user sees "Awareness keeps
        // stopping". The callback also lets us release resources when
        // the user revokes capture from the system UI.
        proj.registerCallback(
            object : MediaProjection.Callback() {
                override fun onStop() {
                    AppLog.i(TAG, "MediaProjection stopped (user revoked or system released)")
                    stop()
                }
            },
            Handler(Looper.getMainLooper()),
        )

        val metrics = DisplayMetrics().also {
            val wm = ctx.getSystemService(Context.WINDOW_SERVICE) as WindowManager
            @Suppress("DEPRECATION")
            wm.defaultDisplay.getRealMetrics(it)
        }
        val w = metrics.widthPixels
        val h = metrics.heightPixels
        reader = ImageReader.newInstance(w, h, PixelFormat.RGBA_8888, 2).also { r ->
            r.setOnImageAvailableListener({ onFrame(it) }, null)
        }

        virtualDisplay = proj.createVirtualDisplay(
            "awareness-capture",
            w, h, metrics.densityDpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
            reader!!.surface,
            null, null,
        )
    }

    companion object {
        private const val TAG = "ScreenCapture"
    }

    private fun onFrame(r: ImageReader) {
        val image: Image = r.acquireLatestImage() ?: return
        try {
            val planes = image.planes
            val buffer = planes[0].buffer
            val pixelStride = planes[0].pixelStride
            val rowStride = planes[0].rowStride
            val rowPadding = rowStride - pixelStride * image.width
            val bitmap = Bitmap.createBitmap(
                image.width + rowPadding / pixelStride,
                image.height,
                Bitmap.Config.ARGB_8888,
            )
            bitmap.copyPixelsFromBuffer(buffer)

            val input = InputImage.fromBitmap(bitmap, 0)
            recognizer.process(input)
                .addOnSuccessListener { latest.set(it.text) }
        } finally {
            image.close()
        }
    }

    fun latestText(): String = latest.get().orEmpty()

    fun stop() {
        virtualDisplay?.release()
        reader?.close()
        projection?.stop()
        recognizer.close()
    }
}
