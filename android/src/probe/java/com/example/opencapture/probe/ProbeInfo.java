package com.example.opencapture.probe;

import android.content.Context;
import android.content.pm.FeatureInfo;
import android.os.Build;
import java.io.BufferedReader;
import java.io.File;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;

/** 读取设备和系统信息。 */
public final class ProbeInfo {
    private ProbeInfo() {}

    /** 设备型号和系统版本，每行一个 "名称=值"。 */
    public static String device(Context context) {
        return "manufacturer=" + Build.MANUFACTURER + "\n"
            + "brand=" + Build.BRAND + "\n"
            + "model=" + Build.MODEL + "\n"
            + "device=" + Build.DEVICE + "\n"
            + "product=" + Build.PRODUCT + "\n"
            + "hardware=" + Build.HARDWARE + "\n"
            + "display=" + Build.DISPLAY + "\n"
            + "android=" + Build.VERSION.RELEASE + " (SDK " + Build.VERSION.SDK_INT + ")\n"
            + "incremental=" + Build.VERSION.INCREMENTAL + "\n"
            + "security_patch=" + Build.VERSION.SECURITY_PATCH + "\n"
            + "fingerprint=" + Build.FINGERPRINT + "\n";
    }

    /** 系统属性里和 PICO、VR、相机相关的条目，每行一个 "名称=值"。 */
    public static String props(Context context) {
        StringBuilder out = new StringBuilder();
        try {
            Process process = Runtime.getRuntime().exec(new String[] {"getprop"});
            BufferedReader reader = new BufferedReader(new InputStreamReader(process.getInputStream(), StandardCharsets.UTF_8));
            String line;
            while ((line = reader.readLine()) != null) {
                String lower = line.toLowerCase();
                if (lower.contains("pico") || lower.contains("pvr") || lower.contains("pxr") || lower.contains("pui")
                    || lower.contains("bytedance") || lower.contains("openxr") || lower.contains("oculus")
                    || lower.contains("ro.build.version") || lower.contains("ro.product.model")
                    || lower.contains("ro.build.display")) {
                    out.append(line.replace("]: [", "=").replace("[", "").replace("]", "")).append('\n');
                }
            }
            process.waitFor();
        } catch (Exception e) {
            out.append("error=").append(e).append('\n');
        }
        return out.toString();
    }

    /** 系统声明的硬件功能里和 VR、相机、Vulkan 相关的条目，每行一个。 */
    public static String features(Context context) {
        StringBuilder out = new StringBuilder();
        FeatureInfo[] features = context.getPackageManager().getSystemAvailableFeatures();
        String[] names = new String[features.length];
        for (int i = 0; i < features.length; i++) {
            FeatureInfo f = features[i];
            names[i] = f.name != null ? f.name + (f.version != 0 ? " v" + Integer.toHexString(f.version) : "") : "";
        }
        Arrays.sort(names);
        for (String name : names) {
            String lower = name.toLowerCase();
            if (lower.contains("vr") || lower.contains("camera") || lower.contains("vulkan") || lower.contains("pico")
                || lower.contains("oculus") || lower.contains("passthrough") || lower.contains("xr")) {
                out.append(name).append('\n');
            }
        }
        return out.toString();
    }

    /** 应用专属外部目录，探测报告会持续写到这里。 */
    public static String filesDir(Context context) {
        File dir = context.getExternalFilesDir(null);
        return dir != null ? dir.getAbsolutePath() : "";
    }
}
