package com.khaen.fixed48test;

import java.io.BufferedInputStream;
import java.io.IOException;
import java.io.InputStream;

/**
 * Minimal streaming RIFF/WAVE parser + signed 16-bit stereo decoder.
 *
 * Supported input: PCM 8/16/24/32-bit or IEEE float32, 1..N channels (first two are used,
 * mono is duplicated). Everything is converted to signed 16-bit stereo for AudioTrack,
 * because 24-bit packed encoding is only available from API 31 and this test targets API 30.
 */
public final class WavFile {

    public final int formatTag;      // 1 = PCM, 3 = IEEE float
    public final int channels;
    public final int sampleRate;
    public final int bitsPerSample;
    public final int blockAlign;
    public final int bytesPerSample;
    public final long dataBytes;     // -1 when unknown (streaming / 0xFFFFFFFF)

    private WavFile(int formatTag, int channels, int sampleRate, int bitsPerSample,
                    int blockAlign, long dataBytes) {
        this.formatTag = formatTag;
        this.channels = channels;
        this.sampleRate = sampleRate;
        this.bitsPerSample = bitsPerSample;
        this.blockAlign = blockAlign;
        this.dataBytes = dataBytes;
        this.bytesPerSample = bitsPerSample / 8;
    }

    public static WavFile parse(InputStream input) throws IOException {
        BufferedInputStream in = (input instanceof BufferedInputStream)
                ? (BufferedInputStream) input
                : new BufferedInputStream(input, 65536);

        byte[] header = new byte[12];
        readFully(in, header, 0, 12);
        if (!"RIFF".equals(ascii(header, 0, 4))) {
            throw new IOException("not a RIFF file (WAV expected)");
        }
        if (!"WAVE".equals(ascii(header, 8, 4))) {
            throw new IOException("RIFF file is not WAVE");
        }

        int formatTag = 1;
        int channels = 0;
        int sampleRate = 0;
        int bitsPerSample = 0;
        int blockAlign = 0;
        boolean haveFmt = false;
        long dataBytes = -1;
        byte[] chunkHeader = new byte[8];

        while (true) {
            readFully(in, chunkHeader, 0, 8);
            String id = ascii(chunkHeader, 0, 4);
            long size = u32(chunkHeader, 4);

            if ("fmt ".equals(id)) {
                if (size < 16 || size > 4096) {
                    throw new IOException("bad fmt chunk size " + size);
                }
                byte[] f = new byte[(int) size];
                readFully(in, f, 0, f.length);
                formatTag = u16(f, 0);
                channels = u16(f, 2);
                sampleRate = (int) u32(f, 4);
                blockAlign = u16(f, 12);
                bitsPerSample = u16(f, 14);
                if (formatTag == 0xFFFE && f.length >= 40) {
                    // WAVE_FORMAT_EXTENSIBLE: real tag is the first 16 bits of the sub-format GUID at +24
                    formatTag = u16(f, 24);
                }
                haveFmt = true;
                if ((size & 1L) == 1L) {
                    skipFully(in, 1);
                }
            } else if ("data".equals(id)) {
                if (!haveFmt) {
                    throw new IOException("data chunk before fmt chunk");
                }
                dataBytes = (size == 0xFFFFFFFFL) ? -1 : size;
                break;
            } else {
                skipFully(in, size);
                if ((size & 1L) == 1L) {
                    skipFully(in, 1);
                }
            }
        }

        if (channels < 1 || channels > 16) {
            throw new IOException("unsupported channel count " + channels);
        }
        if (sampleRate < 4000 || sampleRate > 192000) {
            throw new IOException("unsupported sample rate " + sampleRate);
        }
        if (bitsPerSample != 8 && bitsPerSample != 16 && bitsPerSample != 24 && bitsPerSample != 32) {
            throw new IOException("unsupported bits per sample " + bitsPerSample);
        }
        if (formatTag != 1 && formatTag != 3) {
            throw new IOException("unsupported WAV format tag " + formatTag + " (PCM/float only)");
        }
        if (formatTag == 3 && bitsPerSample != 32) {
            throw new IOException("unsupported float width " + bitsPerSample);
        }
        if (blockAlign <= 0) {
            blockAlign = channels * (bitsPerSample / 8);
        }
        if (blockAlign < channels * (bitsPerSample / 8)) {
            throw new IOException("bad blockAlign " + blockAlign);
        }

        return new WavFile(formatTag, channels, sampleRate, bitsPerSample, blockAlign, dataBytes);
    }

    public String describe() {
        String kind = (formatTag == 3) ? "float" : "PCM";
        return sampleRate + " Hz, " + channels + " ch, " + bitsPerSample + "-bit " + kind;
    }

    /**
     * Decode exactly frames interleaved frames from src (starting at offset 0) into
     * signed 16-bit stereo in dst. dst must hold frames*2 samples.
     */
    public void decode(byte[] src, int frames, short[] dst) {
        boolean isFloat = (formatTag == 3);
        for (int i = 0; i < frames; i++) {
            int base = i * blockAlign;
            int left = sampleAt(src, base, isFloat);
            int right = (channels > 1) ? sampleAt(src, base + bytesPerSample, isFloat) : left;
            dst[2 * i] = (short) left;
            dst[2 * i + 1] = (short) right;
        }
    }

    private int sampleAt(byte[] b, int o, boolean isFloat) {
        switch (bytesPerSample) {
            case 1:
                return ((b[o] & 0xFF) - 128) << 8;
            case 2:
                return (short) ((b[o] & 0xFF) | (b[o + 1] << 8));
            case 3:
                return (((b[o + 2] << 16) | ((b[o + 1] & 0xFF) << 8) | (b[o] & 0xFF)) >> 8);
            case 4: {
                int v = (b[o + 3] << 24) | ((b[o + 2] & 0xFF) << 16)
                        | ((b[o + 1] & 0xFF) << 8) | (b[o] & 0xFF);
                if (isFloat) {
                    float f = Float.intBitsToFloat(v);
                    if (f > 1.0f) f = 1.0f;
                    if (f < -1.0f) f = -1.0f;
                    return Math.round(f * 32767.0f);
                }
                return v >> 16;
            }
            default:
                return 0;
        }
    }

    private static String ascii(byte[] b, int off, int len) {
        StringBuilder sb = new StringBuilder(len);
        for (int i = 0; i < len; i++) {
            sb.append((char) (b[off + i] & 0xFF));
        }
        return sb.toString();
    }

    private static int u16(byte[] b, int off) {
        return (b[off] & 0xFF) | ((b[off + 1] & 0xFF) << 8);
    }

    private static long u32(byte[] b, int off) {
        return ((long) (b[off] & 0xFF))
                | ((long) (b[off + 1] & 0xFF) << 8)
                | ((long) (b[off + 2] & 0xFF) << 16)
                | ((long) (b[off + 3] & 0xFF) << 24);
    }

    private static void readFully(InputStream in, byte[] buf, int off, int len) throws IOException {
        int done = 0;
        while (done < len) {
            int n = in.read(buf, off + done, len - done);
            if (n < 0) {
                throw new IOException("unexpected EOF in WAV header");
            }
            done += n;
        }
    }

    private static void skipFully(InputStream in, long len) throws IOException {
        long left = len;
        byte[] tmp = new byte[65536];
        while (left > 0) {
            int n = in.read(tmp, 0, (int) Math.min(tmp.length, left));
            if (n < 0) {
                throw new IOException("unexpected EOF while skipping WAV chunk");
            }
            left -= n;
        }
    }
}
