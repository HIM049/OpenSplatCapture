package com.example.opencapture;

import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Context;
import android.net.Uri;
import android.provider.MediaStore;
import java.io.BufferedOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.List;
import java.util.zip.Deflater;
import java.util.zip.ZipEntry;
import java.util.zip.ZipOutputStream;

/** 在后台线程把一个会话目录压成 zip，经媒体库写到 Download/OpenCapture/。 */
public final class Exporter {
    public static final int IDLE = 0;
    public static final int RUNNING = 1;
    public static final int DONE = 2;
    public static final int FAILED = 3;

    private final ContentResolver resolver;
    private final Object lock = new Object();
    private int state = IDLE;
    private int percent;
    private String message = "";

    public Exporter(Context context) {
        resolver = context.getContentResolver();
    }

    /** 开始导出；已有导出在进行时返回 false。 */
    public boolean start(final String sessionDir, final String zipName) {
        synchronized (lock) {
            if (state == RUNNING) {
                return false;
            }
            state = RUNNING;
            percent = 0;
            message = "";
        }
        Thread thread = new Thread(new Runnable() {
            @Override
            public void run() {
                export(new File(sessionDir), zipName);
            }
        }, "Exporter");
        thread.start();
        return true;
    }

    private void export(File root, String zipName) {
        Uri uri = null;
        try {
            List<File> files = new ArrayList<>();
            collect(root, files);
            long total = 0;
            for (File file : files) {
                total += file.length();
            }

            ContentValues values = new ContentValues();
            values.put(MediaStore.MediaColumns.DISPLAY_NAME, zipName);
            values.put(MediaStore.MediaColumns.MIME_TYPE, "application/zip");
            values.put(MediaStore.MediaColumns.RELATIVE_PATH, "Download/OpenCapture");
            values.put(MediaStore.MediaColumns.IS_PENDING, 1);
            uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values);
            if (uri == null) {
                throw new IllegalStateException("MediaStore insert returned null");
            }

            String prefix = root.getParentFile().getPath() + File.separator;
            byte[] buffer = new byte[1 << 16];
            long done = 0;
            try (OutputStream raw = resolver.openOutputStream(uri);
                    ZipOutputStream zip = new ZipOutputStream(new BufferedOutputStream(raw, 1 << 16))) {
                zip.setLevel(Deflater.BEST_SPEED);
                for (File file : files) {
                    String name = file.getPath().substring(prefix.length()).replace(File.separatorChar, '/');
                    zip.putNextEntry(new ZipEntry(name));
                    try (FileInputStream in = new FileInputStream(file)) {
                        int read;
                        while ((read = in.read(buffer)) > 0) {
                            zip.write(buffer, 0, read);
                            done += read;
                        }
                    }
                    zip.closeEntry();
                    synchronized (lock) {
                        percent = total == 0 ? 100 : (int) (done * 100 / total);
                    }
                }
            }

            ContentValues publish = new ContentValues();
            publish.put(MediaStore.MediaColumns.IS_PENDING, 0);
            resolver.update(uri, publish, null, null);
            synchronized (lock) {
                state = DONE;
                percent = 100;
                message = "Download/OpenCapture/" + zipName;
            }
        } catch (Exception e) {
            if (uri != null) {
                try {
                    resolver.delete(uri, null, null);
                } catch (Exception ignored) {
                    // 清理失败不影响错误上报。
                }
            }
            synchronized (lock) {
                state = FAILED;
                message = e.toString();
            }
        }
    }

    private static void collect(File dir, List<File> out) {
        File[] children = dir.listFiles();
        if (children == null) {
            return;
        }
        java.util.Arrays.sort(children);
        for (File child : children) {
            if (child.isDirectory()) {
                collect(child, out);
            } else {
                out.add(child);
            }
        }
    }

    public int state() {
        synchronized (lock) {
            return state;
        }
    }

    public int percent() {
        synchronized (lock) {
            return percent;
        }
    }

    /** 成功时是导出文件的相对路径，失败时是错误信息。 */
    public String message() {
        synchronized (lock) {
            return message;
        }
    }
}
