//! OpenXR 与录制格式之间的数值换算。

use std::io::{Read, Write};

use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};

/// OpenXR 右手系（-Z 向前）的位置换算到 Unity 左手系（+Z 向前）。
pub fn position_to_unity(p: [f32; 3]) -> [f32; 3] {
    [p[0], p[1], -p[2]]
}

/// OpenXR 右手系的四元数 (x, y, z, w) 换算到 Unity 左手系。
pub fn rotation_to_unity(q: [f32; 4]) -> [f32; 4] {
    [-q[0], -q[1], q[2], q[3]]
}

/// 16 位归一化深度换算成文件里存的 [0, 1] 浮点值。
pub fn depth_u16_to_normalized(raw: u16) -> f32 {
    raw as f32 / u16::MAX as f32
}

/// 归一化深度换算成沿光轴的线性距离，单位与近远平面相同。
pub fn depth_normalized_to_linear(d: f32, near: f32, far: f32) -> f32 {
    let (x, y) = if far.is_infinite() || far < near {
        (-2.0 * near, -1.0)
    } else {
        (
            -2.0 * far * near / (far - near),
            -(far + near) / (far - near),
        )
    };
    let denom = d * 2.0 - 1.0 + y;
    if denom == 0.0 { 0.0 } else { x / denom }
}

/// 把一层自下而上、行间可能有填充的 16 位深度转成自上而下的小端 float32 字节流。
pub fn depth_layer_to_file_bytes(layer: &[u8], bytes_per_row: usize, width: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(layer.len() / bytes_per_row * width * 4);
    for row in layer.chunks_exact(bytes_per_row).rev() {
        for px in row[..width * 2].chunks_exact(2) {
            let value = depth_u16_to_normalized(u16::from_le_bytes([px[0], px[1]]));
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out
}

/// 把一层自下而上、行间可能有填充的 16 位深度转成自上而下、无填充的小端 16 位字节流。
pub fn depth_layer_to_u16_bytes(layer: &[u8], bytes_per_row: usize, width: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(layer.len() / bytes_per_row * width * 2);
    for row in layer.chunks_exact(bytes_per_row).rev() {
        out.extend_from_slice(&row[..width * 2]);
    }
    out
}

/// 把小端 16 位深度字节流转成文件里存的小端 float32 字节流。
pub fn depth_u16_bytes_to_file_bytes(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 2);
    for px in data.chunks_exact(2) {
        let value = depth_u16_to_normalized(u16::from_le_bytes([px[0], px[1]]));
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// 用 zlib 格式无损压缩。
pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    // 写入 Vec 不会失败。
    let _ = encoder.write_all(data);
    encoder.finish().unwrap_or_default()
}

/// 解开 `compress` 的输出。
pub fn decompress(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    ZlibDecoder::new(data).read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_depth_expands_to_the_raw_file_format() {
        // 3x2 图像，每行 8 字节（含 2 字节填充）。
        let layer: Vec<u8> = (0..16u8).map(|i| i.wrapping_mul(37)).collect();
        let packed = compress(&depth_layer_to_u16_bytes(&layer, 8, 3));
        let expanded = depth_u16_bytes_to_file_bytes(&decompress(&packed).unwrap());
        assert_eq!(expanded, depth_layer_to_file_bytes(&layer, 8, 3));
    }

    #[test]
    fn unity_conversion_mirrors_z() {
        assert_eq!(position_to_unity([1.0, 2.0, -3.0]), [1.0, 2.0, 3.0]);
        // 绕 Y 轴的旋转在镜像 Z 后方向相反。
        assert_eq!(
            rotation_to_unity([0.0, 0.5, 0.0, 0.8]),
            [0.0, -0.5, 0.0, 0.8]
        );
        // 绕 Z 轴的旋转保持不变。
        assert_eq!(
            rotation_to_unity([0.0, 0.0, 0.5, 0.8]),
            [0.0, 0.0, 0.5, 0.8]
        );
    }

    #[test]
    fn linear_depth_with_infinite_far() {
        let near = 0.1;
        assert!((depth_normalized_to_linear(0.0, near, f32::INFINITY) - 0.1).abs() < 1e-6);
        assert!((depth_normalized_to_linear(0.5, near, f32::INFINITY) - 0.2).abs() < 1e-6);
        assert!((depth_normalized_to_linear(0.95, near, f32::INFINITY) - 2.0).abs() < 1e-4);
        assert_eq!(depth_normalized_to_linear(1.0, near, f32::INFINITY), 0.0);
    }

    #[test]
    fn linear_depth_with_finite_far() {
        assert!((depth_normalized_to_linear(0.0, 0.1, 10.0) - 0.1).abs() < 1e-5);
        assert!((depth_normalized_to_linear(1.0, 0.1, 10.0) - 10.0).abs() < 1e-3);
    }

    #[test]
    fn layer_is_flipped_and_unpadded() {
        // 2x2 图像，每行 6 字节（含 2 字节填充），内存中第一行是画面底部。
        let layer = [
            0x00, 0x00, 0xff, 0xff, 0xaa, 0xaa, // 底行: 0, 65535
            0xff, 0xff, 0x00, 0x00, 0xbb, 0xbb, // 顶行: 65535, 0
        ];
        let bytes = depth_layer_to_file_bytes(&layer, 6, 2);
        let values: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        assert_eq!(values, [1.0, 0.0, 0.0, 1.0]);
    }
}
