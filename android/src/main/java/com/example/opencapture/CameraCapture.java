package com.example.opencapture;

import android.content.Context;
import android.graphics.ImageFormat;
import android.graphics.Rect;
import android.graphics.YuvImage;
import android.hardware.camera2.CameraAccessException;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.CaptureResult;
import android.hardware.camera2.TotalCaptureResult;
import android.media.Image;
import android.media.ImageReader;
import android.os.Handler;
import android.os.HandlerThread;
import android.util.Size;
import java.io.BufferedOutputStream;
import java.io.File;
import java.io.FileOutputStream;
import java.lang.reflect.Array;
import java.nio.ByteBuffer;
import java.nio.channels.FileChannel;
import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.Collections;

/** 打开一路透视相机，保留最近几帧；录制期间按请求把时间戳最接近的一帧存成 JPEG 或原样的 YUV。 */
public final class CameraCapture {
    private static final String KEY_SOURCE = "com.meta.extra_metadata.camera_source";
    private static final String KEY_POSITION = "com.meta.extra_metadata.position";
    /** 保留的最近帧数，60 帧/秒下约覆盖 200 毫秒。 */
    private static final int RING_SIZE = 12;

    private final CameraManager manager;
    private final Handler handler;
    private final Object lock = new Object();

    private CameraDevice device;
    private ImageReader reader;
    /** 只在 handler 线程上访问。 */
    private final ArrayDeque<Image> ring = new ArrayDeque<>();
    private long lastSavedTimestamp = -1;

    private String directory;
    private String formatPath;
    private long baseMonoNs;
    private long baseUnixMs;
    /** 大于 0 时存该质量的 JPEG，否则存原始 YUV。 */
    private int jpegQuality;
    /** 只在 handler 线程上访问。 */
    private byte[] nv21;
    private boolean formatWritten;
    private int delivered;
    private int saved;
    /** 相机最近一次上报的曝光时间（纳秒），没有上报时为 0。 */
    private volatile long exposureNs;
    private String error = "";

    public CameraCapture(Context context) {
        manager = (CameraManager) context.getSystemService(Context.CAMERA_SERVICE);
        HandlerThread thread = new HandlerThread("CameraCapture");
        thread.start();
        handler = new Handler(thread.getLooper());
    }

    /** 返回指定位置（0 左，1 右）的透视相机 ID，找不到时返回空串。 */
    public String findCamera(int position) {
        try {
            for (String id : manager.getCameraIdList()) {
                CameraCharacteristics c = manager.getCameraCharacteristics(id);
                if (metaInt(c, KEY_SOURCE) == 0 && metaInt(c, KEY_POSITION) == position) {
                    return id;
                }
            }
        } catch (Exception e) {
            setError("findCamera: " + e);
        }
        return "";
    }

    /**
     * 标定参数，依次为内参 fx fy cx cy s、镜头平移 xyz、镜头旋转 xyzw、
     * 有效像素区 left top right bottom、像素阵列 width height；缺失项为 0。
     */
    public double[] characteristics(String id) {
        double[] out = new double[18];
        try {
            CameraCharacteristics c = manager.getCameraCharacteristics(id);
            copy(c.get(CameraCharacteristics.LENS_INTRINSIC_CALIBRATION), out, 0, 5);
            copy(c.get(CameraCharacteristics.LENS_POSE_TRANSLATION), out, 5, 3);
            copy(c.get(CameraCharacteristics.LENS_POSE_ROTATION), out, 8, 4);
            Rect active = c.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE);
            if (active != null) {
                out[12] = active.left;
                out[13] = active.top;
                out[14] = active.right;
                out[15] = active.bottom;
            }
            Size pixels = c.get(CameraCharacteristics.SENSOR_INFO_PIXEL_ARRAY_SIZE);
            if (pixels != null) {
                out[16] = pixels.getWidth();
                out[17] = pixels.getHeight();
            }
        } catch (Exception e) {
            setError("characteristics: " + e);
        }
        return out;
    }

    /** 以像素阵列尺寸的 YUV_420_888 打开相机并开始连续出帧。 */
    public void open(final String id) {
        handler.post(new Runnable() {
            @Override
            public void run() {
                openNow(id);
            }
        });
    }

    private void openNow(String id) {
        if (device != null || reader != null) {
            return;
        }
        try {
            CameraCharacteristics c = manager.getCameraCharacteristics(id);
            Size size = c.get(CameraCharacteristics.SENSOR_INFO_PIXEL_ARRAY_SIZE);
            reader = ImageReader.newInstance(size.getWidth(), size.getHeight(), ImageFormat.YUV_420_888, RING_SIZE + 2);
            reader.setOnImageAvailableListener(new ImageReader.OnImageAvailableListener() {
                @Override
                public void onImageAvailable(ImageReader r) {
                    onImage(r);
                }
            }, handler);
            manager.openCamera(id, new CameraDevice.StateCallback() {
                @Override
                public void onOpened(CameraDevice camera) {
                    device = camera;
                    startSession();
                }

                @Override
                public void onDisconnected(CameraDevice camera) {
                    setError("camera disconnected");
                    camera.close();
                }

                @Override
                public void onError(CameraDevice camera, int code) {
                    setError("camera error " + code);
                    camera.close();
                }
            }, handler);
        } catch (Exception e) {
            setError("open: " + e);
        }
    }

    private void startSession() {
        try {
            device.createCaptureSession(Collections.singletonList(reader.getSurface()),
                    new CameraCaptureSession.StateCallback() {
                        @Override
                        public void onConfigured(CameraCaptureSession session) {
                            try {
                                CaptureRequest.Builder request =
                                        device.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW);
                                request.addTarget(reader.getSurface());
                                session.setRepeatingRequest(request.build(),
                                        new CameraCaptureSession.CaptureCallback() {
                                            @Override
                                            public void onCaptureCompleted(CameraCaptureSession s,
                                                    CaptureRequest r, TotalCaptureResult result) {
                                                Long exposure = result.get(CaptureResult.SENSOR_EXPOSURE_TIME);
                                                if (exposure != null) {
                                                    exposureNs = exposure;
                                                }
                                            }
                                        }, handler);
                            } catch (CameraAccessException | RuntimeException e) {
                                setError("setRepeatingRequest: " + e);
                            }
                        }

                        @Override
                        public void onConfigureFailed(CameraCaptureSession session) {
                            setError("session configure failed");
                        }
                    }, handler);
        } catch (CameraAccessException | RuntimeException e) {
            setError("createCaptureSession: " + e);
        }
    }

    /**
     * 开始一次录制：之后每次 captureAt 会把选中的帧写到 directory/<unixMs>.jpg（jpegQuality 大于 0）
     * 或 directory/<unixMs>.yuv。
     */
    public void startRecording(String directory, String formatPath, long baseMonoNs, long baseUnixMs,
            int jpegQuality) {
        synchronized (lock) {
            this.jpegQuality = jpegQuality;
            this.directory = directory;
            this.formatPath = formatPath;
            this.baseMonoNs = baseMonoNs;
            this.baseUnixMs = baseUnixMs;
            formatWritten = false;
            saved = 0;
        }
    }

    /** 结束录制；排在它之前的保存请求会先完成。 */
    public void stopRecording() {
        handler.post(new Runnable() {
            @Override
            public void run() {
                synchronized (lock) {
                    directory = null;
                }
            }
        });
    }

    /** 要求保存传感器时间戳最接近 targetMonoNs 的一帧。 */
    public void captureAt(final long targetMonoNs) {
        handler.post(new Runnable() {
            @Override
            public void run() {
                saveNearest(targetMonoNs);
            }
        });
    }

    private void onImage(ImageReader r) {
        Image image;
        try {
            image = r.acquireNextImage();
        } catch (IllegalStateException e) {
            setError("acquireNextImage: " + e);
            return;
        }
        if (image == null) {
            return;
        }
        synchronized (lock) {
            delivered++;
        }
        ring.addLast(image);
        while (ring.size() > RING_SIZE) {
            ring.removeFirst().close();
        }
    }

    private void saveNearest(long targetMonoNs) {
        Image best = null;
        for (Image image : ring) {
            if (best == null
                    || Math.abs(image.getTimestamp() - targetMonoNs) < Math.abs(best.getTimestamp() - targetMonoNs)) {
                best = image;
            }
        }
        if (best == null || best.getTimestamp() == lastSavedTimestamp) {
            return;
        }
        String dir;
        String format;
        long unixMs;
        int quality;
        synchronized (lock) {
            if (directory == null) {
                return;
            }
            dir = directory;
            format = formatWritten ? null : formatPath;
            formatWritten = true;
            quality = jpegQuality;
            unixMs = baseUnixMs + Math.floorDiv(best.getTimestamp() - baseMonoNs, 1_000_000L);
        }
        try {
            Image.Plane[] planes = best.getPlanes();
            if (format != null) {
                writeFormat(format, best, planes, quality);
            }
            if (quality > 0) {
                writeJpeg(new File(dir, unixMs + ".jpg"), best, planes, quality);
            } else {
                try (FileOutputStream out = new FileOutputStream(new File(dir, unixMs + ".yuv"))) {
                    FileChannel channel = out.getChannel();
                    for (Image.Plane plane : planes) {
                        ByteBuffer buffer = plane.getBuffer().duplicate();
                        buffer.rewind();
                        while (buffer.hasRemaining()) {
                            channel.write(buffer);
                        }
                    }
                }
            }
            lastSavedTimestamp = best.getTimestamp();
            synchronized (lock) {
                saved++;
            }
        } catch (Exception e) {
            setError("save frame: " + e);
        }
    }

    /** 把 YUV_420_888 图像排成 NV21 后编码成 JPEG。 */
    private void writeJpeg(File file, Image image, Image.Plane[] planes, int quality) throws Exception {
        int width = image.getWidth();
        int height = image.getHeight();
        int ySize = width * height;
        if (nv21 == null || nv21.length != ySize * 3 / 2) {
            nv21 = new byte[ySize * 3 / 2];
        }
        copyPlane(planes[0], width, height, nv21, 0, 1);
        copyPlane(planes[2], width / 2, height / 2, nv21, ySize, 2);
        copyPlane(planes[1], width / 2, height / 2, nv21, ySize + 1, 2);
        YuvImage yuv = new YuvImage(nv21, ImageFormat.NV21, width, height, null);
        try (BufferedOutputStream out = new BufferedOutputStream(new FileOutputStream(file), 1 << 16)) {
            if (!yuv.compressToJpeg(new Rect(0, 0, width, height), quality, out)) {
                throw new java.io.IOException("compressToJpeg failed");
            }
        }
    }

    /** 把一个平面的 width x height 个采样写到 out[offset + i * step]。 */
    private static void copyPlane(Image.Plane plane, int width, int height, byte[] out, int offset, int step) {
        ByteBuffer buffer = plane.getBuffer().duplicate();
        int rowStride = plane.getRowStride();
        int pixelStride = plane.getPixelStride();
        if (step == 1 && pixelStride == 1 && rowStride == width) {
            buffer.rewind();
            buffer.get(out, offset, width * height);
            return;
        }
        byte[] row = new byte[(width - 1) * pixelStride + 1];
        for (int y = 0; y < height; y++) {
            buffer.position(y * rowStride);
            buffer.get(row, 0, Math.min(row.length, buffer.remaining()));
            int base = offset + y * width * step;
            for (int x = 0; x < width; x++) {
                out[base + x * step] = row[x * pixelStride];
            }
        }
    }

    private void writeFormat(String path, Image image, Image.Plane[] planes, int quality) throws Exception {
        StringBuilder json = new StringBuilder();
        json.append("{\n  \"width\": ").append(image.getWidth())
                .append(",\n  \"height\": ").append(image.getHeight())
                .append(",\n  \"format\": \"YUV_420_888\",\n");
        if (quality > 0) {
            json.append("  \"storage\": \"jpeg\",\n  \"jpegQuality\": ").append(quality).append(",\n");
        }
        json.append("  \"planes\": [\n");
        for (int i = 0; i < planes.length; i++) {
            json.append("    { \"bufferSize\": ").append(planes[i].getBuffer().capacity())
                    .append(", \"rowStride\": ").append(planes[i].getRowStride())
                    .append(", \"pixelStride\": ").append(planes[i].getPixelStride())
                    .append(i + 1 < planes.length ? " },\n" : " }\n");
        }
        json.append("  ],\n  \"baseTime\": { \"baseMonoTimeNs\": ").append(baseMonoNs)
                .append(", \"baseUnixTimeMs\": ").append(baseUnixMs).append(" }\n}\n");
        try (FileOutputStream out = new FileOutputStream(path)) {
            out.write(json.toString().getBytes(StandardCharsets.UTF_8));
        }
    }

    /** 相机累计送达的帧数。 */
    public int deliveredCount() {
        synchronized (lock) {
            return delivered;
        }
    }

    /** 本次录制已写盘的帧数。 */
    public long exposureNs() {
        return exposureNs;
    }

    public int savedCount() {
        synchronized (lock) {
            return saved;
        }
    }

    /** 取出并清空最近一次错误信息。 */
    public String takeError() {
        synchronized (lock) {
            String message = error;
            error = "";
            return message;
        }
    }

    /** 关闭相机并释放保留的帧，之后可以再次 open。 */
    public void close() {
        handler.post(new Runnable() {
            @Override
            public void run() {
                for (Image image : ring) {
                    image.close();
                }
                ring.clear();
                if (device != null) {
                    device.close();
                    device = null;
                }
                if (reader != null) {
                    reader.close();
                    reader = null;
                }
            }
        });
    }

    private void setError(String message) {
        synchronized (lock) {
            error = message;
        }
    }

    private static void copy(Object array, double[] out, int offset, int count) {
        if (array == null || !array.getClass().isArray()) {
            return;
        }
        for (int i = 0; i < count && i < Array.getLength(array); i++) {
            out[offset + i] = ((Number) Array.get(array, i)).doubleValue();
        }
    }

    private static int metaInt(CameraCharacteristics c, String name) {
        for (CameraCharacteristics.Key<?> key : c.getKeys()) {
            if (key.getName().equals(name)) {
                Object value = c.get(key);
                if (value instanceof Number) {
                    return ((Number) value).intValue();
                }
                if (value != null && value.getClass().isArray() && Array.getLength(value) > 0) {
                    Object first = Array.get(value, 0);
                    if (first instanceof Number) {
                        return ((Number) first).intValue();
                    }
                }
            }
        }
        return -1;
    }
}
