use base64::Engine;
use std::collections::HashMap;

const MAX_KITTY_IMAGES: usize = 100;
const MAX_KITTY_CACHE_MB: u64 = 256;

/// 图像格式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
    Rgb,
    Rgba,
}

impl ImageFormat {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "png" => Some(ImageFormat::Png),
            "jpeg" | "jpg" => Some(ImageFormat::Jpeg),
            "webp" => Some(ImageFormat::Webp),
            "rgb" => Some(ImageFormat::Rgb),
            "rgba" => Some(ImageFormat::Rgba),
            _ => None,
        }
    }
}

/// Kitty 图像
#[derive(Debug, Clone)]
pub struct KittyImage {
    pub id: u32,
    #[allow(dead_code)]
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>, // 原始或解码后的图像数据
}

/// Kitty 图像放置
#[derive(Debug, Clone)]
pub struct KittyPlacement {
    pub image_id: u32,
    pub placement_id: Option<u32>,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub z_index: i32,
}

/// Kitty 图像协议参数
#[derive(Debug, Default)]
pub struct KittyGraphicsParams {
    pub action: Option<String>,    // a: t=transfer, d=delete, p=place, q=query
    pub image_id: Option<u32>,     // i
    pub image_number: Option<u32>, // I
    pub placement_id: Option<u32>, // p
    pub format: Option<String>,    // f: png, jpeg, rgb, rgba
    pub width: Option<u32>,        // s
    pub height: Option<u32>,       // v
    pub x: Option<u32>,            // x: column
    pub y: Option<u32>,            // y: row
    pub z: Option<i32>,            // z: z-order
    pub more: bool,                // m: 1=more data, 0=last
    pub data: Option<String>,      // base64 encoded data
}

/// 待传输的图像数据
#[allow(dead_code)]
pub struct PendingTransfer {
    pub image_id: u32,
    pub format: ImageFormat,
    pub chunks: Vec<Vec<u8>>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Kitty 图像协议状态管理
pub struct KittyGraphicsState {
    images: HashMap<u32, KittyImage>,
    placements: Vec<KittyPlacement>,
    pending_transfer: Option<PendingTransfer>,
    next_placement_id: u32,
    total_decoded: u32,
    total_bytes_processed: u64,
    total_image_memory: u64,
    access_order: std::collections::VecDeque<u32>,
}

impl KittyGraphicsState {
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            placements: Vec::new(),
            pending_transfer: None,
            next_placement_id: 1,
            total_decoded: 0,
            total_bytes_processed: 0,
            total_image_memory: 0,
            access_order: std::collections::VecDeque::new(),
        }
    }

    fn enforce_image_limits(&mut self) {
        while self.images.len() > MAX_KITTY_IMAGES
            || self.total_image_memory > MAX_KITTY_CACHE_MB * 1024 * 1024
        {
            if let Some(oldest_id) = self.access_order.pop_front() {
                if let Some(img) = self.images.remove(&oldest_id) {
                    self.total_image_memory -= img.data.len() as u64;
                    self.placements.retain(|p| p.image_id != oldest_id);
                }
            } else {
                break;
            }
        }
    }

    /// 解析 Kitty 图像协议的 DCS 数据
    pub fn parse_graphics_payload(&mut self, payload: &str) -> Result<(), String> {
        let params = Self::parse_params(payload)?;

        match params.action.as_deref() {
            Some("t") | Some("T") => self.handle_transfer(params),
            Some("p") => self.handle_placement(params),
            Some("d") => self.handle_delete(params),
            Some("q") => self.handle_query(params),
            // Continuation chunks of a multi-chunk transfer carry only `m=`/data
            // with no `a=` key, so route them to the transfer handler when a
            // transfer is already in progress.
            None if self.pending_transfer.is_some() => self.handle_transfer(params),
            _ => Err("Unknown action".to_string()),
        }
    }

    /// 解析参数字符串
    fn parse_params(payload: &str) -> Result<KittyGraphicsParams, String> {
        let mut params = KittyGraphicsParams::default();

        // 将 payload 按 ';' 分割
        for pair in payload.split(';') {
            if pair.is_empty() {
                continue;
            }

            // 分割 key=value
            let (key, value) = if let Some(pos) = pair.find('=') {
                (&pair[..pos], &pair[pos + 1..])
            } else {
                (pair, "")
            };

            match key {
                "a" => params.action = Some(value.to_string()),
                "i" => params.image_id = value.parse().ok(),
                "I" => params.image_number = value.parse().ok(),
                "p" => params.placement_id = value.parse().ok(),
                "f" => params.format = Some(value.to_string()),
                "s" => params.width = value.parse().ok(),
                "v" => params.height = value.parse().ok(),
                "x" => params.x = value.parse().ok(),
                "y" => params.y = value.parse().ok(),
                "z" => params.z = value.parse().ok(),
                "m" => params.more = value == "1",
                _ => {
                    // 未知键 = base64 数据段。base64 自身可能含 '=' 填充，
                    // 不能按 key=value 拆分，否则数据会被截断（丢失填充后的内容）。
                    // 因此整个段（含任何 '='）都作为数据。
                    if !pair.is_empty() {
                        params.data = Some(pair.to_string());
                    }
                }
            }
        }

        Ok(params)
    }

    /// 处理传输操作 (a=t)
    fn handle_transfer(&mut self, params: KittyGraphicsParams) -> Result<(), String> {
        // Continuation chunks omit `i=`; fall back to the in-progress transfer's id.
        let image_id = params
            .image_id
            .or_else(|| self.pending_transfer.as_ref().map(|p| p.image_id))
            .ok_or("Missing image ID")?;
        let format_str = params.format.as_deref().unwrap_or("png");
        let format =
            ImageFormat::from_str(format_str).ok_or(format!("Unknown format: {}", format_str))?;

        // 解码 base64 数据
        let data = if let Some(encoded) = params.data {
            let engine = base64::engine::general_purpose::STANDARD;
            engine
                .decode(&encoded)
                .map_err(|e| format!("Base64 decode error: {}", e))?
        } else {
            return Err("No image data provided".to_string());
        };

        if params.more {
            // 分块传输，需要缓存
            let pending = self.pending_transfer.get_or_insert(PendingTransfer {
                image_id,
                format,
                chunks: Vec::new(),
                width: params.width,
                height: params.height,
            });
            pending.chunks.push(data);
        } else {
            // 最后一块或单块传输
            let pending = self.pending_transfer.take();

            // 合并所有块
            let mut final_data = if let Some(pending) = pending {
                let mut combined = Vec::new();
                for chunk in pending.chunks {
                    combined.extend_from_slice(&chunk);
                }
                combined.extend_from_slice(&data);
                combined
            } else {
                data
            };

            // 获取或计算图像尺寸
            let (width, height) = match format {
                ImageFormat::Png | ImageFormat::Jpeg => {
                    // 对于压缩格式，先解码以获取尺寸
                    let (decoded_data, w, h) = self.decode_compressed_image(final_data, format)?;
                    final_data = decoded_data;
                    (w, h)
                }
                ImageFormat::Webp | ImageFormat::Rgb | ImageFormat::Rgba => {
                    // 对于原始格式，必须从参数获取尺寸
                    let w = params.width.ok_or("Missing width for raw image format")?;
                    let h = params.height.ok_or("Missing height for raw image format")?;
                    (w, h)
                }
            };

            let data_size = final_data.len() as u64;
            self.total_decoded += 1;
            self.total_bytes_processed += data_size;
            // Re-transmitting an existing id replaces the old image: drop its
            // memory and its stale access-order entry so the counter doesn't
            // drift (and later underflow in enforce_image_limits).
            if let Some(old) = self.images.get(&image_id) {
                self.total_image_memory = self
                    .total_image_memory
                    .saturating_sub(old.data.len() as u64);
                self.access_order.retain(|&id| id != image_id);
            }
            self.total_image_memory += data_size;
            self.access_order.push_back(image_id);

            self.images.insert(
                image_id,
                KittyImage {
                    id: image_id,
                    format,
                    width,
                    height,
                    data: final_data,
                },
            );

            self.enforce_image_limits();

            log::info!("[KITTY_GRAPHICS] Stored image {} ({}x{}) format: {:?} | Stats: {} images, {}MB total",
                image_id, width, height, format, self.images.len(), self.total_bytes_processed / 1_000_000);
        }

        Ok(())
    }

    /// 解码压缩图像格式（PNG/JPEG），返回 (RGBA数据, 宽度, 高度)
    fn decode_compressed_image(
        &self,
        data: Vec<u8>,
        format: ImageFormat,
    ) -> Result<(Vec<u8>, u32, u32), String> {
        let img =
            image::load_from_memory(&data).map_err(|e| format!("Failed to load image: {}", e))?;

        let width = img.width();
        let height = img.height();
        let rgba_image = img.to_rgba8();

        log::debug!(
            "[KITTY_GRAPHICS] Decoded {:?} image {}x{} -> RGBA {}B",
            format,
            width,
            height,
            rgba_image.len()
        );

        Ok((rgba_image.into_raw(), width, height))
    }

    /// 处理放置操作 (a=p)
    fn handle_placement(&mut self, params: KittyGraphicsParams) -> Result<(), String> {
        let image_id = params.image_id.ok_or("Missing image ID")?;
        let x = params.x.unwrap_or(0);
        let y = params.y.unwrap_or(0);
        let width = params.width.unwrap_or(1);
        let height = params.height.unwrap_or(1);
        let z = params.z.unwrap_or(0);

        let placement_id = params.placement_id.or_else(|| {
            let id = self.next_placement_id;
            self.next_placement_id += 1;
            Some(id)
        });

        self.placements.push(KittyPlacement {
            image_id,
            placement_id,
            x,
            y,
            width,
            height,
            z_index: z,
        });

        // 按 z-order 排序
        self.placements.sort_by_key(|p| p.z_index);

        log::info!(
            "[KITTY_GRAPHICS] Placed image {} at ({},{}) size {}x{} z={}",
            image_id,
            x,
            y,
            width,
            height,
            z
        );

        Ok(())
    }

    /// 处理删除操作 (a=d)
    fn handle_delete(&mut self, params: KittyGraphicsParams) -> Result<(), String> {
        if let Some(image_id) = params.image_id {
            if let Some(img) = self.images.remove(&image_id) {
                self.total_image_memory -= img.data.len() as u64;
            }
            self.placements.retain(|p| p.image_id != image_id);
            self.access_order.retain(|&id| id != image_id);
            log::info!("[KITTY_GRAPHICS] Deleted image {}", image_id);
        } else if let Some(placement_id) = params.placement_id {
            self.placements
                .retain(|p| p.placement_id != Some(placement_id));
            log::info!("[KITTY_GRAPHICS] Deleted placement {}", placement_id);
        } else {
            return Err("Missing image_id or placement_id for delete".to_string());
        }

        Ok(())
    }

    /// 处理查询操作 (a=q)
    fn handle_query(&mut self, _params: KittyGraphicsParams) -> Result<(), String> {
        // 返回支持的格式
        // ESC_DCS ? kitty 0 ; png ; jpeg ; rgb ; rgba ESC_ST
        let response = "\x1bP?kitty 0;png;jpeg;rgb;rgba\x1b\\";
        log::info!("[KITTY_GRAPHICS] Query response: {}", response);
        // 实际应用中需要将此回复发送给应用程序
        Ok(())
    }

    /// 获取性能统计
    #[allow(dead_code)]
    pub fn get_stats(&self) -> (u32, u64, usize) {
        (
            self.total_decoded,
            self.total_bytes_processed,
            self.images.len(),
        )
    }

    /// 获取所有放置
    pub fn get_placements(&self) -> &[KittyPlacement] {
        &self.placements
    }

    /// 获取图像
    pub fn get_image(&self, id: u32) -> Option<&KittyImage> {
        self.images.get(&id)
    }

    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    pub fn image_memory_mb(&self) -> u64 {
        self.total_image_memory / 1_000_000
    }

    /// 清除所有数据
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.images.clear();
        self.placements.clear();
        self.pending_transfer = None;
    }
}

impl Default for KittyGraphicsState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_graphics_params() {
        let payload = "a=t;i=1;s=100;v=100;f=png";
        let params = KittyGraphicsState::parse_params(payload).unwrap();
        assert_eq!(params.action.as_deref(), Some("t"));
        assert_eq!(params.image_id, Some(1));
        assert_eq!(params.width, Some(100));
        assert_eq!(params.height, Some(100));
        assert_eq!(params.format.as_deref(), Some("png"));
    }

    #[test]
    fn test_placement_ordering() {
        let mut state = KittyGraphicsState::new();
        state.placements.push(KittyPlacement {
            image_id: 1,
            placement_id: None,
            x: 0,
            y: 0,
            width: 10,
            height: 10,
            z_index: 5,
        });
        state.placements.push(KittyPlacement {
            image_id: 2,
            placement_id: None,
            x: 10,
            y: 10,
            width: 10,
            height: 10,
            z_index: -1,
        });

        // Sort by z_index
        state.placements.sort_by_key(|p| p.z_index);

        assert_eq!(state.placements[0].z_index, -1);
        assert_eq!(state.placements[1].z_index, 5);
    }

    #[test]
    fn test_complete_kitty_workflow() {
        let mut state = KittyGraphicsState::new();

        // Create a simple 2x2 RGBA image (red square)
        // 4 pixels * 4 bytes (RGBA) = 16 bytes
        let mut image_data = Vec::new();
        for _ in 0..4 {
            image_data.extend_from_slice(&[255, 0, 0, 255]); // Red pixel RGBA
        }

        // Encode to base64
        let base64_data = base64::engine::general_purpose::STANDARD.encode(&image_data);

        println!("Base64 data: {}", base64_data);

        // Simulate receiving image transfer
        // Test 1: Simple parameter test (no data in this call, just verify params)
        let param_test = "a=t;i=1;s=2;v=2;f=rgba;m=0";
        match KittyGraphicsState::parse_params(param_test) {
            Ok(params) => {
                println!("Parsed params - action: {:?}, id: {:?}, w: {:?}, h: {:?}, fmt: {:?}, more: {}, data: {:?}",
                    params.action, params.image_id, params.width, params.height, params.format, params.more, params.data);
            }
            Err(e) => {
                println!("Parse params error: {}", e);
            }
        }

        // Now test with data
        let payload = format!("a=t;i=1;s=2;v=2;f=rgba;m=0;{}", base64_data);
        println!("Full payload: {}", payload);

        // Try parsing the full payload
        match state.parse_graphics_payload(&payload) {
            Ok(_) => {
                println!("Successfully parsed and processed image");
            }
            Err(e) => {
                println!("Full parse error: {}", e);
            }
        }
    }
}
