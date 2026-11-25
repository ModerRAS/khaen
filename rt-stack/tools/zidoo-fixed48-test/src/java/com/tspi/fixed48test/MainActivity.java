package com.tspi.fixed48test;

import android.Manifest;
import android.app.Activity;
import android.content.ContentUris;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.graphics.Color;
import android.graphics.Typeface;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioTrack;
import android.media.PlaybackParams;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.MediaStore;
import android.util.Log;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.view.WindowManager;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import java.io.BufferedInputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.text.SimpleDateFormat;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
import java.util.Date;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Locale;
import java.util.Set;

/**
 * Zidoo fixed-48k AudioTrack probe.
 *
 * Two buttons create a MODE_STREAM AudioTrack at 48 kHz and 44.1 kHz and write a 1 kHz
 * sine for N seconds. The goal is to observe (from the MOTU TOSLink receiver) whether the
 * S/PDIF output rate follows the AudioTrack rate or is forced by the box.
 *
 * Everything is deliberately dependency free: no AndroidX, no resources, no native code.
 */
public class MainActivity extends Activity {

    private static final String TAG = "Fixed48Test";
    private static final int REQ_READ_STORAGE = 1001;
    private static final double TONE_HZ = 1000.0;
    private static final double AMPLITUDE = 0.5;   // -6 dBFS

    private TextView statusView;
    private TextView detailsView;
    private Button durationButton;

    private volatile boolean playing = false;
    private Thread playThread;
    private int durationSeconds = 10;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        setContentView(buildUi());
        refreshDetails();
        setStatusNow("IDLE - press a button.\n\n"
                + "Protocol: install, stop tspi-rt-host on the host, arm arecord on MOTU ch12/13,\n"
                + "then press \"Play 48 kHz tone\". See zidoo-apk48/PROTOCOL.md.");
    }

    // ------------------------------------------------------------------ UI

    private ScrollView buildUi() {
        ScrollView scroll = new ScrollView(this);
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setPadding(dp(28), dp(20), dp(28), dp(24));
        root.setBackgroundColor(Color.parseColor("#0B0F14"));

        TextView title = new TextView(this);
        title.setText("Zidoo Fixed-48k AudioTrack Test");
        title.setTextSize(TypedValue.COMPLEX_UNIT_SP, 30);
        title.setTypeface(Typeface.DEFAULT_BOLD);
        title.setTextColor(Color.WHITE);
        root.addView(title);

        TextView sub = new TextView(this);
        sub.setText("com.tspi.fixed48test v1.0 - does the AudioTrack sample rate control S/PDIF/TOSLink?");
        sub.setTextSize(TypedValue.COMPLEX_UNIT_SP, 14);
        sub.setTextColor(Color.parseColor("#8FB3C8"));
        sub.setPadding(0, dp(4), 0, dp(16));
        root.addView(sub);

        root.addView(makeButton("Play 48 kHz tone (1 kHz sine)", "#1B6E8C", new View.OnClickListener() {
            public void onClick(View v) {
                startTone(48000);
            }
        }));
        root.addView(makeButton("Play 44.1 kHz tone (1 kHz sine)", "#8C5A1B", new View.OnClickListener() {
            public void onClick(View v) {
                startTone(44100);
            }
        }));
        root.addView(makeButton("Play file (WAV from Music / MediaStore)", "#2E6E3E", new View.OnClickListener() {
            public void onClick(View v) {
                startWav();
            }
        }));
        root.addView(makeButton("Stop / release AudioTrack", "#555F6E", new View.OnClickListener() {
            public void onClick(View v) {
                stopAndReport();
            }
        }));

        durationButton = makeButton("", "#33414F", new View.OnClickListener() {
            public void onClick(View v) {
                cycleDuration();
            }
        });
        updateDurationButton();
        root.addView(durationButton);

        statusView = new TextView(this);
        statusView.setTypeface(Typeface.MONOSPACE);
        statusView.setTextSize(TypedValue.COMPLEX_UNIT_SP, 16);
        statusView.setTextColor(Color.parseColor("#C8F0FF"));
        statusView.setBackgroundColor(Color.parseColor("#101820"));
        statusView.setPadding(dp(16), dp(14), dp(16), dp(14));
        LinearLayout.LayoutParams statusLp = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        statusLp.topMargin = dp(16);
        root.addView(statusView, statusLp);

        detailsView = new TextView(this);
        detailsView.setTextSize(TypedValue.COMPLEX_UNIT_SP, 13);
        detailsView.setTextColor(Color.parseColor("#7FA0B5"));
        detailsView.setPadding(0, dp(14), 0, 0);
        root.addView(detailsView);

        scroll.addView(root);
        return scroll;
    }

    private Button makeButton(String text, String colorHex, View.OnClickListener l) {
        Button b = new Button(this);
        b.setText(text);
        b.setTextSize(TypedValue.COMPLEX_UNIT_SP, 22);
        b.setAllCaps(false);
        b.setTextColor(Color.WHITE);
        b.setBackgroundColor(Color.parseColor(colorHex));
        b.setGravity(Gravity.CENTER);
        b.setMinHeight(dp(64));
        b.setPadding(dp(16), dp(12), dp(16), dp(12));
        b.setOnClickListener(l);
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        lp.bottomMargin = dp(10);
        b.setLayoutParams(lp);
        return b;
    }

    private int dp(int v) {
        return (int) (v * getResources().getDisplayMetrics().density + 0.5f);
    }

    private void cycleDuration() {
        if (durationSeconds == 10) {
            durationSeconds = 30;
        } else if (durationSeconds == 30) {
            durationSeconds = 60;
        } else {
            durationSeconds = 10;
        }
        updateDurationButton();
    }

    private void updateDurationButton() {
        if (durationButton != null) {
            durationButton.setText("Tone duration: " + durationSeconds + " s  (tap to change)");
        }
    }

    private void refreshDetails() {
        detailsView.setText(deviceInfo());
    }

    private String deviceInfo() {
        AudioManager am = (AudioManager) getSystemService(AUDIO_SERVICE);
        String mix = "?";
        String frames = "?";
        if (am != null) {
            mix = String.valueOf(am.getProperty(AudioManager.PROPERTY_OUTPUT_SAMPLE_RATE));
            frames = String.valueOf(am.getProperty(AudioManager.PROPERTY_OUTPUT_FRAMES_PER_BUFFER));
        }
        StringBuilder abis = new StringBuilder();
        for (String abi : Build.SUPPORTED_ABIS) {
            if (abis.length() > 0) {
                abis.append(", ");
            }
            abis.append(abi);
        }
        File dir = getExternalFilesDir(null);
        String logPath = ((dir == null ? "?" : dir.getAbsolutePath()) + "/fixed48test-results.jsonl");
        return "Device: " + Build.MANUFACTURER + " " + Build.MODEL
                + " | Android " + Build.VERSION.RELEASE + " (SDK " + Build.VERSION.SDK_INT + ")\n"
                + "AudioFlinger mix: " + mix + " Hz, " + frames + " frames/buffer\n"
                + "ABIs: " + abis + " | package: " + getPackageName() + " v1.0\n"
                + "Result log: " + logPath;
    }

    // ------------------------------------------------------------- playback

    private AudioTrack createTrack(int rate) {
        AudioAttributes attrs = new AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                .build();
        int minBuf = AudioTrack.getMinBufferSize(rate, AudioFormat.CHANNEL_OUT_STEREO,
                AudioFormat.ENCODING_PCM_16BIT);
        int framesPerChunk = Math.max(256, rate / 20);      // 50 ms chunks
        int wantBytes = framesPerChunk * 4;                // stereo, 16-bit
        int bufBytes = (minBuf > 0) ? Math.max(minBuf * 2, wantBytes) : wantBytes;

        AudioTrack track = new AudioTrack.Builder()
                .setAudioAttributes(attrs)
                .setAudioFormat(new AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(rate)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                        .build())
                .setTransferMode(AudioTrack.MODE_STREAM)
                .setBufferSizeInBytes(bufBytes)
                .build();
        if (track.getState() != AudioTrack.STATE_INITIALIZED) {
            int st = track.getState();
            track.release();
            throw new IllegalStateException("AudioTrack state=" + st + " (rate " + rate + ")");
        }
        return track;
    }

    private void startTone(final int requestedRate) {
        stopPlayback();
        final int seconds = durationSeconds;
        playing = true;
        setStatusNow(toneStatus(requestedRate, -1, -1, "n/a", "creating AudioTrack ..."));
        Thread t = new Thread(new Runnable() {
            public void run() {
                runTone(requestedRate, seconds);
            }
        }, "tone-" + requestedRate);
        playThread = t;
        t.start();
    }

    private void runTone(int requestedRate, int seconds) {
        AudioTrack track = null;
        int trackRate = -1;
        int formatRate = -1;
        int underruns = -1;
        long framesWritten = 0;
        String params = "n/a";
        String error = null;
        long startedNs = System.nanoTime();

        try {
            track = createTrack(requestedRate);
            trackRate = track.getSampleRate();
            formatRate = track.getFormat().getSampleRate();
            try {
                PlaybackParams pp = track.getPlaybackParams();
                params = String.format(Locale.US, "speed=%.3f pitch=%.3f", pp.getSpeed(), pp.getPitch());
            } catch (Throwable t) {
                params = "unavailable";
            }
            postStatus(toneStatus(requestedRate, trackRate, formatRate, params,
                    "playing 0/" + seconds + " s"));
            track.play();

            final int framesPerChunk = Math.max(256, requestedRate / 20);
            final short[] chunk = new short[framesPerChunk * 2];
            final long targetFrames = (long) requestedRate * seconds;
            final double inc = 2.0 * Math.PI * TONE_HZ / requestedRate;
            double phase = 0.0;
            long lastSecond = -1;

            while (playing && framesWritten < targetFrames) {
                int n = (int) Math.min(framesPerChunk, targetFrames - framesWritten);
                for (int i = 0; i < n; i++) {
                    short v = (short) Math.round(AMPLITUDE * 32767.0 * Math.sin(phase));
                    chunk[2 * i] = v;
                    chunk[2 * i + 1] = v;
                    phase += inc;
                    if (phase >= 2.0 * Math.PI) {
                        phase -= 2.0 * Math.PI;
                    }
                }
                int written = track.write(chunk, 0, n * 2);
                if (written < 0) {
                    error = "AudioTrack.write returned " + written;
                    break;
                }
                framesWritten += written / 2;
                long second = framesWritten / requestedRate;
                if (second != lastSecond) {
                    lastSecond = second;
                    postStatus(toneStatus(requestedRate, trackRate, formatRate, params,
                            "playing " + second + "/" + seconds + " s"));
                }
            }
            underruns = track.getUnderrunCount();
        } catch (Throwable t) {
            error = String.valueOf(t);
            Log.e(TAG, "tone failed", t);
        } finally {
            if (track != null) {
                try {
                    track.stop();
                } catch (Throwable ignored) {
                }
                try {
                    track.release();
                } catch (Throwable ignored) {
                }
            }
            playing = false;
        }

        double elapsed = (System.nanoTime() - startedNs) / 1e9;
        String state;
        if (error != null) {
            state = "ERROR: " + error;
        } else if (framesWritten < (long) requestedRate * seconds) {
            state = String.format(Locale.US, "STOPPED after %.2f s", elapsed);
        } else {
            state = String.format(Locale.US, "DONE %.2f s, frames=%d, underruns=%d",
                    elapsed, framesWritten, underruns);
        }
        postStatus(toneStatus(requestedRate, trackRate, formatRate, params, state));
        appendResult("tone", String.format(Locale.US,
                "\"requestedRate\":%d,\"trackRate\":%d,\"formatRate\":%d,"
                        + "\"deviceMixRate\":\"%s\",\"deviceFramesPerBuffer\":\"%s\","
                        + "\"durationSeconds\":%.3f,\"frames\":%d,\"underruns\":%d,"
                        + "\"params\":\"%s\",\"error\":%s",
                requestedRate, trackRate, formatRate, mixRate(), framesPerBuffer(),
                elapsed, framesWritten, underruns,
                params, error == null ? "null" : "\"" + sanitize(error) + "\""));
    }

    private String toneStatus(int requested, int trackRate, int formatRate,
                              String params, String state) {
        StringBuilder sb = new StringBuilder();
        sb.append("=== 1 kHz sine via AudioTrack, requested ").append(requested).append(" Hz ===\n");
        sb.append(String.format(Locale.US, "%-16s: %6d Hz\n", "requested rate", requested));
        sb.append(String.format(Locale.US, "%-16s: %6s Hz   (getSampleRate)\n", "AudioTrack rate",
                trackRate > 0 ? String.valueOf(trackRate) : "?"));
        sb.append(String.format(Locale.US, "%-16s: %6s Hz   (getFormat)\n", "format rate",
                formatRate > 0 ? String.valueOf(formatRate) : "?"));
        sb.append(String.format(Locale.US, "%-16s: %6s Hz   (AudioManager)\n", "device mix rate",
                mixRate()));
        sb.append(String.format(Locale.US, "%-16s: %s\n", "playback params", params));
        sb.append(String.format(Locale.US, "%-16s: %s\n", "state", state));
        if (trackRate > 0) {
            sb.append("verdict         : ");
            if (trackRate == requested) {
                sb.append("MATCH - AudioTrack accepted the requested rate\n");
            } else {
                sb.append("MISMATCH - AudioTrack reported ").append(trackRate)
                        .append(" Hz for a ").append(requested).append(" Hz request\n");
            }
        }
        return sb.toString();
    }

    // ------------------------------------------------------------- WAV file

    private void startWav() {
        if (!ensureStoragePermission()) {
            postStatus("Storage permission requested.\n"
                    + "Press Allow, then press \"Play file (WAV ...)\" again.");
            return;
        }
        stopPlayback();
        playing = true;
        setStatusNow("Scanning Music folders and MediaStore for .wav ...");
        Thread t = new Thread(new Runnable() {
            public void run() {
                scanAndPlayWav();
            }
        }, "wav");
        playThread = t;
        t.start();
    }

    private void scanAndPlayWav() {
        List<WavSource> sources = new ArrayList<WavSource>();
        try {
            findWavSources(sources);
        } catch (Throwable t) {
            Log.w(TAG, "WAV scan failed", t);
        }
        if (sources.isEmpty()) {
            playing = false;
            postStatus("No .wav file found (Music folder + MediaStore).\n\n"
                    + "Expected path on this box: /sdcard/Music/tspi-toslink-48k-24s.wav\n"
                    + "Copy a 16/24-bit WAV there and press \"Play file\" again.");
            appendResult("wav", "\"result\":\"no-file\"");
            return;
        }
        WavSource src = sources.get(0);
        playWavSource(src, sources.size());
    }

    private void findWavSources(List<WavSource> out) {
        queryMediaStore(out);
        if (!out.isEmpty()) {
            return;
        }
        String[] dirs = {
                "/storage/emulated/0/Music",
                "/sdcard/Music",
                "/storage/emulated/0/Download",
                "/sdcard/Download"
        };
        Set<String> seen = new LinkedHashSet<String>();
        for (String dirPath : dirs) {
            File dir = new File(dirPath);
            if (!dir.isDirectory()) {
                continue;
            }
            File[] files;
            try {
                files = dir.listFiles();
            } catch (Throwable t) {
                files = null;
            }
            if (files == null) {
                continue;
            }
            Arrays.sort(files, new Comparator<File>() {
                public int compare(File a, File b) {
                    return a.getName().compareToIgnoreCase(b.getName());
                }
            });
            for (File f : files) {
                if (!f.isFile()) {
                    continue;
                }
                String name = f.getName();
                if (!name.toLowerCase(Locale.US).endsWith(".wav")) {
                    continue;
                }
                if (!seen.add(name)) {
                    continue;
                }
                out.add(new WavSource(name, f.getAbsolutePath(), f, null));
            }
        }
    }

    private void queryMediaStore(List<WavSource> out) {
        Set<String> seen = new LinkedHashSet<String>();
        Cursor c = null;
        try {
            String[] proj = {MediaStore.Audio.Media._ID, MediaStore.Audio.Media.DISPLAY_NAME};
            c = getContentResolver().query(MediaStore.Audio.Media.EXTERNAL_CONTENT_URI, proj,
                    MediaStore.Audio.Media.DISPLAY_NAME + " LIKE ?", new String[]{"%.wav"},
                    MediaStore.Audio.Media._ID + " DESC");
            while (c != null && c.moveToNext()) {
                long id = c.getLong(0);
                String name = c.getString(1);
                if (name == null || !seen.add(name)) {
                    continue;
                }
                Uri uri = ContentUris.withAppendedId(MediaStore.Audio.Media.EXTERNAL_CONTENT_URI, id);
                out.add(new WavSource(name, uri.toString(), null, uri));
            }
        } catch (Throwable t) {
            Log.w(TAG, "MediaStore audio query failed", t);
        } finally {
            if (c != null) {
                c.close();
            }
        }
        if (!out.isEmpty()) {
            return;
        }
        c = null;
        try {
            Uri files = MediaStore.Files.getContentUri("external");
            String[] proj = {MediaStore.Files.FileColumns._ID, MediaStore.Files.FileColumns.DISPLAY_NAME};
            c = getContentResolver().query(files, proj,
                    MediaStore.Files.FileColumns.DISPLAY_NAME + " LIKE ?", new String[]{"%.wav"},
                    null);
            while (c != null && c.moveToNext()) {
                long id = c.getLong(0);
                String name = c.getString(1);
                if (name == null || !seen.add(name)) {
                    continue;
                }
                Uri uri = ContentUris.withAppendedId(files, id);
                out.add(new WavSource(name, uri.toString(), null, uri));
            }
        } catch (Throwable t) {
            Log.w(TAG, "MediaStore files query failed", t);
        } finally {
            if (c != null) {
                c.close();
            }
        }
    }

    private void playWavSource(WavSource src, int count) {
        InputStream in = null;
        AudioTrack track = null;
        WavFile wav = null;
        int trackRate = -1;
        int underruns = -1;
        long framesDone = 0;
        long totalFrames = -1;
        String error = null;
        long startedNs = System.nanoTime();

        try {
            in = src.open(this);
            wav = WavFile.parse(in);
            totalFrames = (wav.dataBytes > 0) ? (wav.dataBytes / wav.blockAlign) : -1;
            track = createTrack(wav.sampleRate);
            trackRate = track.getSampleRate();
            postStatus(wavStatus(src, count, wav, trackRate, framesDone, totalFrames,
                    underruns, "starting"));
            track.play();

            int chunkFrames = Math.min(Math.max(256, wav.sampleRate / 20), 8192);
            byte[] raw = new byte[chunkFrames * wav.blockAlign];
            short[] pcm = new short[chunkFrames * 2];
            long softLimit = (totalFrames > 0) ? totalFrames : (long) wav.sampleRate * 120;
            long lastSecond = -1;

            while (playing && framesDone < softLimit) {
                int got = readSome(in, raw, raw.length);
                if (got <= 0) {
                    break;
                }
                int frames = got / wav.blockAlign;
                if (frames <= 0) {
                    break;
                }
                wav.decode(raw, frames, pcm);
                int written = track.write(pcm, 0, frames * 2);
                if (written < 0) {
                    error = "AudioTrack.write returned " + written;
                    break;
                }
                framesDone += written / 2;
                long second = framesDone / wav.sampleRate;
                if (second != lastSecond) {
                    lastSecond = second;
                    postStatus(wavStatus(src, count, wav, trackRate, framesDone, totalFrames,
                            underruns, "playing"));
                }
            }
            underruns = track.getUnderrunCount();
        } catch (Throwable t) {
            error = String.valueOf(t);
            Log.e(TAG, "wav failed", t);
        } finally {
            if (track != null) {
                try {
                    track.stop();
                } catch (Throwable ignored) {
                }
                try {
                    track.release();
                } catch (Throwable ignored) {
                }
            }
            if (in != null) {
                try {
                    in.close();
                } catch (Throwable ignored) {
                }
            }
            playing = false;
        }

        double elapsed = (System.nanoTime() - startedNs) / 1e9;
        String state;
        if (error != null) {
            state = "ERROR: " + error;
        } else if (totalFrames > 0 && framesDone < totalFrames) {
            state = String.format(Locale.US, "STOPPED/EOF after %.2f s", elapsed);
        } else {
            state = String.format(Locale.US, "DONE %.2f s, frames=%d, underruns=%d",
                    elapsed, framesDone, underruns);
        }
        postStatus(wavStatus(src, count, wav, trackRate, framesDone, totalFrames, underruns, state));
        if (wav != null) {
            appendResult("wav", String.format(Locale.US,
                    "\"file\":\"%s\",\"fileRate\":%d,\"fileChannels\":%d,\"fileBits\":%d,"
                            + "\"trackRate\":%d,\"durationSeconds\":%.3f,\"frames\":%d,"
                            + "\"underruns\":%d,\"error\":%s",
                    sanitize(src.label), wav.sampleRate, wav.channels, wav.bitsPerSample,
                    trackRate, elapsed, framesDone, underruns,
                    error == null ? "null" : "\"" + sanitize(error) + "\""));
        } else {
            appendResult("wav", "\"error\":\"" + sanitize(error == null ? "no data" : error) + "\"");
        }
    }

    private String wavStatus(WavSource src, int count, WavFile wav, int trackRate,
                             long framesDone, long totalFrames, int underruns, String state) {
        StringBuilder sb = new StringBuilder();
        sb.append("=== WAV file via AudioTrack ===\n");
        sb.append(String.format(Locale.US, "%-16s: %s\n", "file", src.label));
        sb.append(String.format(Locale.US, "%-16s: %s\n", "path/uri", src.pathOrUri));
        if (wav != null) {
            sb.append(String.format(Locale.US, "%-16s: %s\n", "WAV format", wav.describe()));
        }
        sb.append(String.format(Locale.US, "%-16s: %6s Hz   (getSampleRate)\n", "AudioTrack rate",
                trackRate > 0 ? String.valueOf(trackRate) : "?"));
        if (totalFrames > 0 && wav != null) {
            double dur = (double) totalFrames / wav.sampleRate;
            double pos = (double) framesDone / wav.sampleRate;
            sb.append(String.format(Locale.US, "%-16s: %.1f s / %.1f s\n", "position", pos, dur));
        }
        if (underruns >= 0) {
            sb.append(String.format(Locale.US, "%-16s: %d\n", "underruns", underruns));
        }
        if (count > 1) {
            sb.append(String.format(Locale.US, "%-16s: %d (using first)\n", "wav candidates", count));
        }
        sb.append(String.format(Locale.US, "%-16s: %s\n", "state", state));
        return sb.toString();
    }

    // ------------------------------------------------------------- helpers

    private boolean ensureStoragePermission() {
        if (checkSelfPermission(Manifest.permission.READ_EXTERNAL_STORAGE)
                != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[]{Manifest.permission.READ_EXTERNAL_STORAGE},
                    REQ_READ_STORAGE);
            return false;
        }
        return true;
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions,
                                           int[] grantResults) {
        if (requestCode == REQ_READ_STORAGE) {
            boolean ok = grantResults.length > 0
                    && grantResults[0] == PackageManager.PERMISSION_GRANTED;
            if (ok) {
                postStatus("Storage permission granted.\nPress \"Play file (WAV ...)\" again.");
            } else {
                postStatus("Storage permission denied.\nWAV playback is unavailable; the tone buttons still work.");
            }
        }
    }

    private void stopPlayback() {
        playing = false;
        Thread t = playThread;
        playThread = null;
        if (t != null && t != Thread.currentThread()) {
            try {
                t.join(3000);
            } catch (InterruptedException ignored) {
                Thread.currentThread().interrupt();
            }
        }
    }

    private void stopAndReport() {
        boolean wasPlaying = playing;
        stopPlayback();
        setStatusNow("STOP requested. AudioTrack stopped and released"
                + (wasPlaying ? "." : " (nothing was playing).")
                + "\n\n" + deviceInfo());
    }

    @Override
    protected void onDestroy() {
        stopPlayback();
        super.onDestroy();
    }

    private String mixRate() {
        AudioManager am = (AudioManager) getSystemService(AUDIO_SERVICE);
        if (am == null) {
            return "?";
        }
        String v = am.getProperty(AudioManager.PROPERTY_OUTPUT_SAMPLE_RATE);
        return (v == null) ? "?" : v;
    }

    private String framesPerBuffer() {
        AudioManager am = (AudioManager) getSystemService(AUDIO_SERVICE);
        if (am == null) {
            return "?";
        }
        String v = am.getProperty(AudioManager.PROPERTY_OUTPUT_FRAMES_PER_BUFFER);
        return (v == null) ? "?" : v;
    }

    private void setStatusNow(String text) {
        if (statusView != null) {
            statusView.setText(text);
        }
    }

    private void postStatus(final String text) {
        runOnUiThread(new Runnable() {
            public void run() {
                setStatusNow(text);
            }
        });
    }

    private static int readSome(InputStream in, byte[] buf, int len) throws IOException {
        int off = 0;
        while (off < len) {
            int n = in.read(buf, off, len - off);
            if (n < 0) {
                break;
            }
            off += n;
        }
        return off;
    }

    private static String sanitize(String s) {
        if (s == null) {
            return "";
        }
        return s.replace('"', '\'').replace('\n', ' ').replace('\r', ' ');
    }

    private static String nowIso() {
        SimpleDateFormat f = new SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss.SSSZ", Locale.US);
        return f.format(new Date());
    }

    private synchronized void appendResult(String tag, String fields) {
        File dir = getExternalFilesDir(null);
        if (dir == null) {
            dir = getFilesDir();
        }
        if (dir == null) {
            return;
        }
        if (!dir.exists() && !dir.mkdirs()) {
            return;
        }
        FileOutputStream fos = null;
        try {
            File f = new File(dir, "fixed48test-results.jsonl");
            String line = "{\"ts\":\"" + nowIso() + "\",\"tag\":\"" + tag + "\"," + fields + "}\n";
            fos = new FileOutputStream(f, true);
            fos.write(line.getBytes(StandardCharsets.UTF_8));
            fos.flush();
            Log.i(TAG, "RESULT " + line.trim());
        } catch (Throwable t) {
            Log.w(TAG, "appendResult failed", t);
        } finally {
            if (fos != null) {
                try {
                    fos.close();
                } catch (Throwable ignored) {
                }
            }
        }
    }

    private static final class WavSource {
        final String label;
        final String pathOrUri;
        final File file;
        final Uri uri;

        WavSource(String label, String pathOrUri, File file, Uri uri) {
            this.label = label;
            this.pathOrUri = pathOrUri;
            this.file = file;
            this.uri = uri;
        }

        InputStream open(Activity activity) throws IOException {
            if (file != null) {
                return new BufferedInputStream(new FileInputStream(file), 65536);
            }
            if (uri != null) {
                InputStream in = activity.getContentResolver().openInputStream(uri);
                if (in == null) {
                    throw new IOException("cannot open " + uri);
                }
                return new BufferedInputStream(in, 65536);
            }
            throw new IOException("no file source");
        }
    }
}
