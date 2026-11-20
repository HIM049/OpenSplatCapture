package com.example.opencapture.probe;

import android.content.Context;
import android.graphics.ImageFormat;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.SystemClock;
import android.util.Size;
import java.nio.ByteBuffer;
import java.util.Arrays;
import java.util.Collections;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/** 用 Android 相机接口列出所有相机，并尝试从指定相机取一帧。 */
public final class ProbeCamera {
    /** 缩略图的宽度（像素），高度按画面比例计算。 */
    private static final int THUMB_WIDTH = 240;

    private static volatile String lastError = "";
    private static volatile String lastInfo = "";

    private ProbeCamera() {}

    /** 最近一次 grab 失败的原因。 */
    public static String lastError() {
        return lastError;
    }

    /** 最近一次 grab 成功时的画面信息。 */
    public static String lastInfo() {
        return lastInfo;
    }

    /** 所有相机 ID，用逗号分隔；出错时返回以 "error:" 开头的文字。 */
    public static String ids(Context context) {
        try {
            CameraManager manager = (CameraManager) context.getSystemService(Context.CAMERA_SERVICE);
            return String.join(",", manager.getCameraIdList());
        } catch (Exception e) {
            return "error: " + e;
        }
    }

    /** 一个相机的特性：朝向、YUV 分辨率、标定参数和所有特性键名，每行一项。 */
    public static String describe(Context context, String id) {
        StringBuilder out = new StringBuilder();
        try {
            CameraManager manager = (CameraManager) context.getSystemService(Context.CAMERA_SERVICE);
            CameraCharacteristics c = manager.getCameraCharacteristics(id);
            Integer facing = c.get(CameraCharacteristics.LENS_FACING);
            out.append("facing=").append(facing == null ? "?" : facing == 0 ? "front" : facing == 1 ? "back" : "external").append('\n');
            StreamConfigurationMap map = c.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
            if (map != null) {
                Size[] sizes = map.getOutputSizes(ImageFormat.YUV_420_888);
                out.append("yuv=").append(sizes == null ? "none" : Arrays.toString(sizes)).append('\n');
            }
            Size pixels = c.get(CameraCharacteristics.SENSOR_INFO_PIXEL_ARRAY_SIZE);
            out.append("pixel_array=").append(pixels).append('\n');
            out.append("intrinsics=").append(Arrays.toString(c.get(CameraCharacteristics.LENS_INTRINSIC_CALIBRATION))).append('\n');
            out.append("pose_translation=").append(Arrays.toString(c.get(CameraCharacteristics.LENS_POSE_TRANSLATION))).append('\n');
            out.append("pose_rotation=").append(Arrays.toString(c.get(CameraCharacteristics.LENS_POSE_ROTATION))).append('\n');
            Integer reference = c.get(CameraCharacteristics.LENS_POSE_REFERENCE);
            out.append("pose_reference=").append(reference).append('\n');
            out.append("physical_ids=").append(c.getPhysicalCameraIds()).append('\n');
            StringBuilder keys = new StringBuilder();
            for (CameraCharacteristics.Key<?> key : c.getKeys()) {
                String name = key.getName();
                if (!name.startsWith("android.")) {
                    keys.append(name).append(' ');
                }
            }
            out.append("vendor_keys=").append(keys.toString().trim()).append('\n');
        } catch (Exception e) {
            out.append("error=").append(e).append('\n');
        }
        return out.toString();
    }

    /**
     * 以最小的 YUV 分辨率打开相机，等第一帧到达后关闭；
     * 成功时返回亮度缩略图的 RGBA 像素，失败时返回 null，原因见 lastError。
     */
    public static byte[] grab(Context context, String id, long timeoutMs) {
        lastError = "";
        lastInfo = "";
        HandlerThread thread = new HandlerThread("ProbeCamera");
        thread.start();
        Handler handler = new Handler(thread.getLooper());
        final CameraDevice[] device = new CameraDevice[1];
        final byte[][] result = new byte[1][];
        final CountDownLatch done = new CountDownLatch(1);
        ImageReader reader = null;
        long start = SystemClock.elapsedRealtime();
        try {
            CameraManager manager = (CameraManager) context.getSystemService(Context.CAMERA_SERVICE);
            CameraCharacteristics c = manager.getCameraCharacteristics(id);
            StreamConfigurationMap map = c.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
            Size[] sizes = map == null ? null : map.getOutputSizes(ImageFormat.YUV_420_888);
            if (sizes == null || sizes.length == 0) {
                lastError = "no YUV_420_888 output";
                return null;
            }
            Size size = Collections.min(Arrays.asList(sizes), (a, b) -> a.getWidth() * a.getHeight() - b.getWidth() * b.getHeight());
            final ImageReader imageReader = ImageReader.newInstance(size.getWidth(), size.getHeight(), ImageFormat.YUV_420_888, 2);
            reader = imageReader;
            imageReader.setOnImageAvailableListener(r -> {
                Image image = r.acquireLatestImage();
                if (image == null) {
                    return;
                }
                try {
                    if (result[0] == null) {
                        result[0] = thumbnail(image);
                        lastInfo = image.getWidth() + "x" + image.getHeight() + " YUV_420_888, first frame after "
                            + (SystemClock.elapsedRealtime() - start) + " ms";
                        done.countDown();
                    }
                } catch (Exception e) {
                    fail("reading frame: " + e, done);
                } finally {
                    image.close();
                }
            }, handler);
            manager.openCamera(id, new CameraDevice.StateCallback() {
                @Override
                public void onOpened(CameraDevice camera) {
                    device[0] = camera;
                    try {
                        camera.createCaptureSession(Collections.singletonList(imageReader.getSurface()), new CameraCaptureSession.StateCallback() {
                            @Override
                            public void onConfigured(CameraCaptureSession session) {
                                try {
                                    CaptureRequest.Builder request = camera.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW);
                                    request.addTarget(imageReader.getSurface());
                                    session.setRepeatingRequest(request.build(), null, handler);
                                } catch (Exception e) {
                                    fail("repeating request: " + e, done);
                                }
                            }

                            @Override
                            public void onConfigureFailed(CameraCaptureSession session) {
                                fail("capture session configuration failed", done);
                            }
                        }, handler);
                    } catch (Exception e) {
                        fail("create session: " + e, done);
                    }
                }

                @Override
                public void onDisconnected(CameraDevice camera) {
                    fail("disconnected", done);
                }

                @Override
                public void onError(CameraDevice camera, int error) {
                    fail("camera error " + error, done);
                }
            }, handler);
            if (!done.await(timeoutMs, TimeUnit.MILLISECONDS)) {
                lastError = "no frame within " + timeoutMs + " ms";
            }
            return result[0];
        } catch (Exception e) {
            lastError = e.toString();
            return null;
        } finally {
            // 在回调所在的线程上关闭，避免回调读帧时缓冲被释放。
            final ImageReader toClose = reader;
            CountDownLatch closed = new CountDownLatch(1);
            handler.post(() -> {
                if (device[0] != null) {
                    device[0].close();
                }
                if (toClose != null) {
                    toClose.close();
                }
                closed.countDown();
            });
            try {
                closed.await(2, TimeUnit.SECONDS);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
            thread.quitSafely();
        }
    }

    private static void fail(String message, CountDownLatch done) {
        if (lastError.isEmpty()) {
            lastError = message;
        }
        done.countDown();
    }

    /** 把 Y 平面缩放成宽 THUMB_WIDTH 的灰度 RGBA 图。 */
    private static byte[] thumbnail(Image image) {
        Image.Plane plane = image.getPlanes()[0];
        ByteBuffer buffer = plane.getBuffer();
        int rowStride = plane.getRowStride();
        int pixelStride = plane.getPixelStride();
        int width = image.getWidth();
        int height = image.getHeight();
        int thumbHeight = Math.max(1, height * THUMB_WIDTH / width);
        byte[] out = new byte[THUMB_WIDTH * thumbHeight * 4];
        for (int y = 0; y < thumbHeight; y++) {
            int sy = y * height / thumbHeight;
            for (int x = 0; x < THUMB_WIDTH; x++) {
                int sx = x * width / THUMB_WIDTH;
                byte v = buffer.get(sy * rowStride + sx * pixelStride);
                int o = (y * THUMB_WIDTH + x) * 4;
                out[o] = v;
                out[o + 1] = v;
                out[o + 2] = v;
                out[o + 3] = (byte) 255;
            }
        }
        return out;
    }
}
