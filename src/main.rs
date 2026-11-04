#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod update;

use chrono::Local;
use eframe::egui::{
    self, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2,
};
use image::{codecs::gif::GifDecoder, AnimationDecoder};
use nokhwa::{
    pixel_format::RgbFormat,
    utils::{CameraIndex, RequestedFormat, RequestedFormatType, Resolution},
    Camera,
};
use rodio::{source::SineWave, DeviceSinkBuilder, Source};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::BufReader,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
        Arc,
    },
    thread,
    time::Duration,
};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const CORE_LICENSE_TEXT: &str = include_str!("../LICENSE");
const BUG_REPORT_URL: &str =
    "https://github.com/DataLayerHost/ScanBarcode/issues/new?template=bug_report.yml";
const FEATURE_REQUEST_URL: &str =
    "https://github.com/DataLayerHost/ScanBarcode/issues/new?template=feature_request.yml";

const SUPPORTED_CODE_TYPES: [&str; 19] = [
    "QR",
    "Micro QR",
    "Data Matrix",
    "Aztec",
    "PDF417",
    "MaxiCode",
    "Codabar",
    "Code 39",
    "Code 93",
    "Code 128",
    "EAN-8",
    "EAN-13",
    "ITF",
    "UPC-A",
    "UPC-E",
    "RSS-14",
    "RSS Expanded",
    "Telepen",
    "UPC/EAN Extension",
];
struct ScanBarcode {
    camera_on: bool,
    torch_on: bool,
    camera_index: usize,
    camera_count: usize,
    camera_names: Vec<String>,
    scanned_text: String,
    scanned_items: Vec<String>,
    selected_scan: Option<usize>,
    dragged_scan: Option<usize>,
    last_action: Option<String>,
    last_deeplink: Option<String>,
    last_camera_value: Option<String>,
    menu_open: bool,
    mirror_camera: bool,
    show_preview: bool,
    mute_sound: bool,
    toast: Option<String>,
    camera_command: Option<Sender<CameraCommand>>,
    camera_frames: Option<Receiver<CameraFrame>>,
    camera_texture: Option<egui::TextureHandle>,
    camera_error: Option<String>,
    confirm_clear: bool,
    confirm_close: bool,
    close_confirmed: bool,
    about_open: bool,
    auto_updates: bool,
    update_rx: Option<Receiver<UpdateMessage>>,
    update_status: String,
    update_prompt: bool,
    update_phase: UpdatePhase,
    update_version: Option<String>,
    update_progress: Option<f32>,
    update_downloaded: u64,
    update_total: Option<u64>,
    update_archive: Option<PathBuf>,
    update_partial: Option<PathBuf>,
    update_pause: Option<Arc<AtomicBool>>,
    update_asset_url: Option<String>,
    update_digest: Option<[u8; 32]>,
    barcode_tx: BarcodeTxDecoder,
}

enum CameraCommand {
    SetCamera(usize),
    SetRunning(bool),
}
struct CameraFrame {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
    codes: Vec<DetectedCode>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct DetectedCode {
    text: String,
    format: String,
    raw_bytes: Vec<u8>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum UpdatePhase {
    Idle,
    Checking,
    Available,
    Downloading,
    Pausing,
    Paused,
    Ready,
    Installing,
    Error,
}

use update::UpdateMessage;

#[derive(Default)]
struct BarcodeTxDecoder {
    transfer_id: Option<[u8; 6]>,
    payload_length: usize,
    block_size: usize,
    source_block_count: usize,
    symbol_ids: HashSet<u32>,
    rows: HashMap<usize, FountainEquation>,
    result: Option<BarcodeTxSummary>,
}

struct FountainEquation {
    coefficients: Vec<bool>,
    value: Vec<u8>,
}

#[derive(Clone, Debug)]
struct BarcodeTxSummary {
    transactions: usize,
    supported_xcb: usize,
    items: Vec<String>,
}

impl BarcodeTxDecoder {
    fn is_active(&self) -> bool {
        self.transfer_id.is_some() && self.result.is_none()
    }
    fn progress(&self) -> f32 {
        if self.source_block_count == 0 {
            0.0
        } else {
            self.rows.len() as f32 / self.source_block_count as f32
        }
    }
    fn received_symbols(&self) -> usize {
        self.symbol_ids.len()
    }
    fn reset(&mut self) {
        *self = Self::default();
    }
    fn add_frame(&mut self, bytes: &[u8]) -> Result<bool, String> {
        if self.result.is_some() {
            return Ok(false);
        }
        let frame = BarcodeTxFrame::parse(bytes)?;
        if let Some(transfer_id) = self.transfer_id {
            if transfer_id != frame.transfer_id {
                return Ok(false);
            }
        } else {
            self.transfer_id = Some(frame.transfer_id);
            self.payload_length = frame.payload_length;
            self.block_size = frame.block_size;
            self.source_block_count = frame.source_block_count;
        }
        if self.payload_length != frame.payload_length
            || self.block_size != frame.block_size
            || self.source_block_count != frame.source_block_count
        {
            return Err("Inconsistent BarcodeTx frame metadata".into());
        }
        if !self.symbol_ids.insert(frame.symbol_id) {
            return Ok(false);
        }
        if self.symbol_ids.len() > 8192 {
            return Err("BarcodeTx frame limit exceeded".into());
        }
        let selected =
            select_source_blocks(frame.symbol_id, self.source_block_count, &frame.transfer_id);
        if selected.len() != frame.degree {
            return Err("Invalid BarcodeTx fountain degree".into());
        }
        let mut coefficients = vec![false; self.source_block_count];
        for index in selected {
            coefficients[index] = true;
        }
        let mut value = frame.symbol;
        loop {
            let Some(pivot) = coefficients.iter().position(|set| *set) else {
                if value.iter().any(|byte| *byte != 0) {
                    return Err("Inconsistent BarcodeTx frame".into());
                }
                break;
            };
            if let Some(existing) = self.rows.get(&pivot) {
                for index in 0..coefficients.len() {
                    coefficients[index] ^= existing.coefficients[index];
                    value[index] ^= existing.value[index];
                }
            } else {
                self.rows.insert(
                    pivot,
                    FountainEquation {
                        coefficients,
                        value,
                    },
                );
                break;
            }
        }
        if self.rows.len() == self.source_block_count {
            self.finish()?;
        }
        Ok(true)
    }
    fn finish(&mut self) -> Result<(), String> {
        let mut solved: Vec<Option<Vec<u8>>> = vec![None; self.source_block_count];
        for pivot in (0..self.source_block_count).rev() {
            let equation = self
                .rows
                .get(&pivot)
                .ok_or_else(|| "BarcodeTx reconstruction is incomplete".to_string())?;
            let mut value = equation.value.clone();
            for index in pivot + 1..self.source_block_count {
                if equation.coefficients[index] {
                    let block = solved[index]
                        .as_ref()
                        .ok_or_else(|| "BarcodeTx reconstruction failed".to_string())?;
                    xor_in_place(&mut value, block);
                }
            }
            solved[pivot] = Some(value);
        }
        let mut protected = Vec::with_capacity(self.source_block_count * self.block_size);
        for block in solved.into_iter().flatten() {
            protected.extend(block);
        }
        protected.truncate(self.payload_length);
        if protected.len() < 33 {
            return Err("BarcodeTx payload is too short".into());
        }
        let digest_offset = protected.len() - 32;
        let batch = &protected[..digest_offset];
        let supplied = &protected[digest_offset..];
        let digest = Sha256::digest(batch);
        if supplied != digest.as_slice()
            || self
                .transfer_id
                .as_ref()
                .is_none_or(|id| id != &digest[..6])
        {
            return Err("BarcodeTx integrity check failed".into());
        }
        self.result = Some(parse_barcode_tx_batch(batch)?);
        Ok(())
    }
}

struct BarcodeTxFrame {
    transfer_id: [u8; 6],
    payload_length: usize,
    block_size: usize,
    source_block_count: usize,
    symbol_id: u32,
    degree: usize,
    symbol: Vec<u8>,
}

impl BarcodeTxFrame {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 27 || bytes.len() > 800 || &bytes[..2] != b"BX" {
            return Err("Not a BarcodeTx frame".into());
        }
        if bytes[2] != 1 || bytes[3] != 0 {
            return Err("Unsupported BarcodeTx frame".into());
        }
        let expected_crc = read_u32(&bytes[bytes.len() - 4..]);
        if crc32(&bytes[..bytes.len() - 4]) != expected_crc {
            return Err("BarcodeTx frame checksum failed".into());
        }
        let payload_length = read_u32(&bytes[10..14]) as usize;
        let block_size = read_u16(&bytes[14..16]) as usize;
        let source_block_count = read_u16(&bytes[16..18]) as usize;
        let symbol_id = read_u32(&bytes[18..22]);
        let degree = bytes[22] as usize;
        if payload_length == 0
            || payload_length > 1_048_608
            || block_size == 0
            || block_size > 773
            || bytes.len() != 27 + block_size
        {
            return Err("Invalid BarcodeTx frame limits".into());
        }
        let expected_count = payload_length.div_ceil(block_size);
        if source_block_count == 0
            || source_block_count != expected_count
            || degree == 0
            || degree > source_block_count.min(8)
        {
            return Err("Invalid BarcodeTx fountain metadata".into());
        }
        let mut transfer_id = [0u8; 6];
        transfer_id.copy_from_slice(&bytes[4..10]);
        Ok(Self {
            transfer_id,
            payload_length,
            block_size,
            source_block_count,
            symbol_id,
            degree,
            symbol: bytes[23..bytes.len() - 4].to_vec(),
        })
    }
}

fn select_source_blocks(symbol_id: u32, source_count: usize, transfer_id: &[u8; 6]) -> Vec<usize> {
    if (symbol_id as usize) < source_count {
        return vec![symbol_id as usize];
    }
    let mut state = symbol_id.wrapping_mul(0x9e37_79b1);
    for (index, byte) in transfer_id.iter().enumerate() {
        state ^= u32::from(*byte) << ((index & 3) * 8);
    }
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let maximum_degree = source_count.min(8);
    let degree = if source_count == 1 {
        1
    } else {
        2 + (next() as usize % (maximum_degree - 1))
    };
    let mut selected = HashSet::new();
    while selected.len() < degree {
        selected.insert(next() as usize % source_count);
    }
    let mut selected: Vec<_> = selected.into_iter().collect();
    selected.sort_unstable();
    selected
}

fn parse_barcode_tx_batch(bytes: &[u8]) -> Result<BarcodeTxSummary, String> {
    if bytes.len() < 7 || &bytes[..4] != b"BTXB" || bytes[4] != 1 {
        return Err("Invalid BarcodeTx batch".into());
    }
    let count = read_u16(&bytes[5..7]) as usize;
    if count == 0 || count > 256 {
        return Err("Invalid BarcodeTx transaction count".into());
    }
    let mut offset = 7;
    let mut supported_xcb = 0;
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        if offset + 8 > bytes.len() {
            return Err("Truncated BarcodeTx transaction".into());
        }
        let blockchain = &bytes[offset..offset + 3];
        offset += 3;
        let flags = bytes[offset];
        offset += 1;
        if flags & !1 != 0 {
            return Err("Unsupported BarcodeTx flags".into());
        }
        if flags & 1 != 0 {
            if offset + 4 > bytes.len() {
                return Err("Truncated BarcodeTx network".into());
            }
            offset += 4;
        }
        let length = read_u32(&bytes[offset..offset + 4]) as usize;
        offset += 4;
        if length == 0 || length > 512 * 1024 || offset + length > bytes.len() {
            return Err("Invalid BarcodeTx transaction length".into());
        }
        if blockchain.eq_ignore_ascii_case(b"XCB") {
            supported_xcb += 1;
        }
        items.push(format!(
            "BarcodeTx {} transaction: {}",
            String::from_utf8_lossy(blockchain),
            bytes_to_hex(&bytes[offset..offset + length])
        ));
        offset += length;
    }
    if offset != bytes.len() {
        return Err("Trailing BarcodeTx batch data".into());
    }
    Ok(BarcodeTxSummary {
        transactions: count,
        supported_xcb,
        items,
    })
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn xor_in_place(left: &mut [u8], right: &[u8]) {
    for (left, right) in left.iter_mut().zip(right) {
        *left ^= right;
    }
}

fn read_u16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc ^ u32::MAX
}

impl ScanBarcode {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        #[cfg(target_os = "macos")]
        install_native_menu();
        let camera_names = detect_camera_names();
        let camera_count = camera_names.len();
        let storage = cc.storage;
        let camera_on = storage
            .and_then(|s| s.get_string("camera_on"))
            .map(|v| v == "true")
            .unwrap_or(true);
        let saved_index = storage
            .and_then(|s| s.get_string("camera_index"))
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let mirror_camera = storage
            .and_then(|s| s.get_string("mirror_camera"))
            .map(|v| v != "false")
            .unwrap_or(true);
        let auto_updates = storage
            .and_then(|s| s.get_string("auto_updates"))
            .map(|v| v != "false")
            .unwrap_or(true);
        let show_preview = storage
            .and_then(|s| s.get_string("show_preview"))
            .map(|v| v != "false")
            .unwrap_or(true);
        let mute_sound = storage
            .and_then(|s| s.get_string("mute_sound"))
            .map(|v| v == "true")
            .unwrap_or(false);
        let mut app = Self {
            camera_on,
            torch_on: false,
            camera_index: saved_index.min(camera_count.saturating_sub(1)),
            camera_count,
            camera_names,
            scanned_text: String::new(),
            scanned_items: Vec::new(),
            selected_scan: None,
            dragged_scan: None,
            last_action: None,
            last_deeplink: None,
            last_camera_value: None,
            menu_open: false,
            mirror_camera,
            show_preview,
            mute_sound,
            toast: None,
            camera_command: None,
            camera_frames: None,
            camera_texture: None,
            camera_error: None,
            confirm_clear: false,
            confirm_close: false,
            close_confirmed: false,
            about_open: false,
            auto_updates,
            update_rx: None,
            update_status: format!("Version {APP_VERSION}"),
            update_prompt: false,
            update_phase: UpdatePhase::Idle,
            update_version: None,
            update_progress: None,
            update_downloaded: 0,
            update_total: None,
            update_archive: None,
            update_partial: None,
            update_pause: None,
            update_asset_url: None,
            update_digest: None,
            barcode_tx: BarcodeTxDecoder::default(),
        };
        if app.camera_on {
            app.start_camera();
        }
        if app.auto_updates {
            app.start_update_check();
        }
        app
    }
    fn say(&mut self, message: impl Into<String>) {
        self.toast = Some(message.into());
    }
    fn record_scan(
        &mut self,
        value: impl Into<String>,
        action: Option<String>,
        deeplink: Option<String>,
    ) {
        let value = value.into();
        if value.is_empty() {
            return;
        }
        self.scanned_items.push(value);
        self.selected_scan = Some(self.scanned_items.len() - 1);
        self.sync_scanned_text();
        self.last_action = action;
        self.last_deeplink = deeplink;
    }
    fn select_scan(&mut self, index: usize) {
        if index < self.scanned_items.len() {
            self.selected_scan = Some(index);
            self.refresh_selected_action();
        }
    }
    fn sync_scanned_text(&mut self) {
        self.scanned_text = self.scanned_items.join("\n");
    }
    fn remove_scan(&mut self, index: usize) {
        if index < self.scanned_items.len() {
            self.scanned_items.remove(index);
            self.selected_scan = match self.selected_scan {
                Some(selected) if selected == index => self.scanned_items.len().checked_sub(1),
                Some(selected) if selected > index => Some(selected - 1),
                other => other,
            };
            self.sync_scanned_text();
            self.refresh_selected_action();
        }
    }
    fn move_scan(&mut self, from: usize, to: usize) {
        if from < self.scanned_items.len() && to < self.scanned_items.len() && from != to {
            let item = self.scanned_items.remove(from);
            self.scanned_items.insert(to, item);
            self.selected_scan = self.selected_scan.map(|selected| {
                if selected == from {
                    to
                } else if from < selected && to >= selected {
                    selected - 1
                } else if from > selected && to <= selected {
                    selected + 1
                } else {
                    selected
                }
            });
            self.sync_scanned_text();
            self.refresh_selected_action();
        }
    }
    fn move_selected_scan(&mut self, direction: isize) {
        if let Some(from) = self.selected_scan {
            if let Some(to) = from.checked_add_signed(direction) {
                self.move_scan(from, to);
            }
        }
    }
    fn refresh_selected_action(&mut self) {
        (self.last_action, self.last_deeplink) = self
            .selected_scan
            .and_then(|index| self.scanned_items.get(index))
            .map(|item| classify_scan(item))
            .unwrap_or((None, None));
    }
    fn record_decoded(&mut self, code: DetectedCode) {
        if is_barcode_tx_frame(&code.raw_bytes) {
            self.record_barcode_tx_frame(&code.raw_bytes);
            return;
        }
        let (action, deeplink) = classify_scan(&code.text);
        self.record_scan(code.text, action, deeplink);
        if !self.mute_sound {
            play_scan_beep();
        }
    }
    fn record_camera_decoded(&mut self, code: DetectedCode) {
        if is_barcode_tx_frame(&code.raw_bytes) {
            self.record_barcode_tx_frame(&code.raw_bytes);
            return;
        }
        if self.last_camera_value.as_deref() == Some(code.text.as_str()) {
            return;
        }
        self.last_camera_value = Some(code.text.clone());
        self.record_decoded(code);
    }
    fn record_barcode_tx_frame(&mut self, bytes: &[u8]) {
        match self.barcode_tx.add_frame(bytes) {
            Ok(true) => {
                if let Some(result) = self.barcode_tx.result.clone() {
                    for item in &result.items {
                        self.record_scan(item.clone(), None, None);
                    }
                    self.say(format!(
                        "BarcodeTx complete: {} transaction{} ({} XCB)",
                        result.transactions,
                        if result.transactions == 1 { "" } else { "s" },
                        result.supported_xcb
                    ));
                }
            }
            Ok(false) => {}
            Err(error) => self.say(error),
        }
    }
    fn copy(&mut self) {
        if self.scanned_text.is_empty() {
            self.say("Nothing to copy");
        } else if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(self.scanned_text.clone());
            self.say("Copied scan results");
        }
    }
    fn clear(&mut self) {
        self.scanned_text.clear();
        self.scanned_items.clear();
        self.selected_scan = None;
        self.dragged_scan = None;
        self.last_action = None;
        self.last_deeplink = None;
        self.last_camera_value = None;
        self.barcode_tx.reset();
        self.say("Results cleared");
    }
    fn request_clear(&mut self) {
        if !self.scanned_text.is_empty() {
            self.confirm_clear = true;
        }
    }
    fn request_close(&mut self, ctx: &egui::Context) {
        if self.scanned_items.is_empty() || self.close_confirmed {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            self.confirm_close = true;
        }
    }
    fn start_update_check(&mut self) {
        let (tx, rx) = mpsc::channel();
        self.update_rx = Some(rx);
        thread::spawn(move || {
            let message = update::check_release().unwrap_or_else(UpdateMessage::Error);
            let _ = tx.send(message);
        });
        self.update_phase = UpdatePhase::Checking;
        self.update_archive = None;
        self.update_partial = None;
        self.update_pause = None;
        self.update_asset_url = None;
        self.update_digest = None;
        self.update_version = None;
        self.update_progress = None;
        self.update_status = "Checking for updates…".into();
    }
    fn poll_updates(&mut self) {
        let messages: Vec<_> = self
            .update_rx
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for message in messages {
            match message {
                UpdateMessage::Available {
                    version,
                    asset_url,
                    sha256,
                } => {
                    if is_newer_version(&version, APP_VERSION) {
                        self.update_phase = if asset_url.is_some() && sha256.is_some() {
                            UpdatePhase::Available
                        } else {
                            UpdatePhase::Error
                        };
                        self.update_status = if asset_url.is_none() {
                            "No compatible package was found for this computer".into()
                        } else if sha256.is_none() {
                            "GitHub did not provide a valid SHA-256 digest for this package".into()
                        } else {
                            format!("Version {version} is ready to download")
                        };
                        self.update_prompt = true;
                        self.update_version = Some(version);
                        self.update_asset_url = asset_url;
                        self.update_digest = sha256;
                    } else {
                        self.update_status = format!("Version {APP_VERSION} is latest");
                        self.update_phase = UpdatePhase::Idle;
                    }
                }
                UpdateMessage::Progress { downloaded, total } => {
                    self.update_downloaded = downloaded;
                    self.update_total = total;
                    self.update_progress = total
                        .filter(|total| *total > 0)
                        .map(|total| downloaded as f32 / total as f32);
                    if self.update_phase == UpdatePhase::Downloading {
                        self.update_status = "Downloading update…".into();
                    }
                }
                UpdateMessage::Downloaded(path) => {
                    self.update_archive = Some(path);
                    self.update_partial = None;
                    self.update_pause = None;
                    self.update_phase = UpdatePhase::Ready;
                    self.update_status = "Download complete — SHA-256 verified".into();
                    self.update_prompt = true;
                }
                UpdateMessage::Paused {
                    path,
                    downloaded,
                    total,
                } => {
                    self.update_partial = Some(path);
                    self.update_pause = None;
                    self.update_downloaded = downloaded;
                    self.update_total = total;
                    self.update_progress = total
                        .filter(|total| *total > 0)
                        .map(|total| downloaded as f32 / total as f32);
                    self.update_phase = UpdatePhase::Paused;
                    self.update_status = "Download paused. Continue whenever you're ready.".into();
                    self.update_prompt = true;
                }
                UpdateMessage::Prepared(path) => match update::launch_install_helper(&path) {
                    Ok(()) => std::process::exit(0),
                    Err(error) => self.update_error(error),
                },
                UpdateMessage::Error(error) => self.update_error(error),
            }
        }
    }
    fn update_error(&mut self, error: String) {
        if self.update_phase != UpdatePhase::Checking {
            self.update_prompt = true;
        }
        self.update_phase = UpdatePhase::Error;
        self.update_status = error;
    }
    fn apply_update(&mut self) {
        let Some(url) = self.update_asset_url.clone() else {
            self.update_error("No compatible package was found for this computer".into());
            return;
        };
        let Some(sha256) = self.update_digest else {
            self.update_error(
                "GitHub did not provide a valid SHA-256 digest for this package".into(),
            );
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.update_rx = Some(rx);
        self.update_phase = UpdatePhase::Downloading;
        let partial = self.update_partial.take();
        if partial.is_none() {
            self.update_progress = None;
            self.update_downloaded = 0;
            self.update_total = None;
        }
        self.update_status = "Downloading update…".into();
        let pause = Arc::new(AtomicBool::new(false));
        self.update_pause = Some(pause.clone());
        thread::spawn(move || {
            let message = match update::download_asset(&url, sha256, partial, &pause, &tx) {
                Ok(update::DownloadOutcome::Downloaded(path)) => UpdateMessage::Downloaded(path),
                Ok(update::DownloadOutcome::Paused {
                    path,
                    downloaded,
                    total,
                }) => UpdateMessage::Paused {
                    path,
                    downloaded,
                    total,
                },
                Err(error) => UpdateMessage::Error(error),
            };
            let _ = tx.send(message);
        });
    }
    fn pause_update(&mut self) {
        if let Some(pause) = &self.update_pause {
            pause.store(true, Ordering::Relaxed);
            self.update_phase = UpdatePhase::Pausing;
            self.update_status = "Pausing download…".into();
        }
    }
    fn install_update(&mut self) {
        let Some(archive) = self.update_archive.clone() else {
            self.update_error("Download the update before installing it".into());
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.update_rx = Some(rx);
        self.update_phase = UpdatePhase::Installing;
        self.update_status = "Preparing installation…".into();
        thread::spawn(move || {
            let message = match update::prepare_install(&archive) {
                Ok(path) => UpdateMessage::Prepared(path),
                Err(error) => UpdateMessage::Error(error),
            };
            let _ = tx.send(message);
        });
    }
    fn download(&mut self) {
        if self.scanned_text.is_empty() {
            self.say("Nothing to download");
            return;
        }
        let filename = format!("scanbarcode-{}.txt", Local::now().format("%Y-%m-%d-%H%M%S"));
        if let Some(path) = rfd::FileDialog::new()
            .set_file_name(&filename)
            .add_filter("Text file", &["txt"])
            .save_file()
        {
            match fs::write(&path, &self.scanned_text) {
                Ok(()) => self.say(format!(
                    "Downloaded {}",
                    path.file_name()
                        .map(|name| name.to_string_lossy())
                        .unwrap_or_else(|| filename.into())
                )),
                Err(_) => self.say("Could not write text file"),
            }
        }
    }
    fn execute(&mut self) {
        let Some(target) = self.last_deeplink.clone() else {
            return;
        };
        match self.last_action.as_deref() {
            Some("Import exchange rates") => {
                let count = count_rate_entries(&target);
                self.say(format!(
                    "Recognized {count} exchange rate{}",
                    if count == 1 { "" } else { "s" }
                ));
            }
            Some("Connect to Wi-Fi") => {
                if execute_wifi_payload(&target) {
                    self.say("Wi-Fi connection requested");
                } else {
                    self.say("Could not read Wi-Fi details");
                }
            }
            Some("Add contact") => {
                if execute_vcard(&target) {
                    self.say("Opened contact card");
                } else {
                    self.say("Could not open contact card");
                }
            }
            Some("Import contact") | Some("Import business contact") => {
                if execute_structured_contact(&target) {
                    self.say("Opened contact card");
                } else {
                    self.say("Could not open contact card");
                }
            }
            Some("Compose email") => {
                if execute_email_payload(&target) {
                    self.say("Opened email composer");
                } else {
                    self.say("Could not read email details");
                }
            }
            Some("Compose SMS") | Some("Compose multimedia message") => {
                if execute_sms_payload(&target) {
                    self.say("Opened message composer");
                } else {
                    self.say("Could not read message details");
                }
            }
            Some("Add calendar event") => {
                if execute_calendar_payload(&target) {
                    self.say("Opened calendar event");
                } else {
                    self.say("Could not open calendar event");
                }
            }
            _ => {
                open_external(&target);
                self.say(format!(
                    "Opened {}",
                    target.split(':').next().unwrap_or("link")
                ));
            }
        }
    }
    fn selected_camera_name(&self) -> &str {
        self.camera_names
            .get(self.camera_index)
            .map(String::as_str)
            .unwrap_or("Camera")
    }
    fn start_camera(&mut self) {
        #[cfg(target_os = "macos")]
        {
            nokhwa::nokhwa_initialize(|_| {});
        }
        let (command_tx, command_rx) = mpsc::channel();
        let (frame_tx, frame_rx) = mpsc::sync_channel(1);
        let index = self.camera_index;
        thread::spawn(move || camera_loop(index, command_rx, frame_tx));
        self.camera_command = Some(command_tx);
        self.camera_frames = Some(frame_rx);
        self.camera_error = None;
    }
    fn set_camera_running(&mut self, running: bool) {
        if let Some(command) = &self.camera_command {
            let _ = command.send(CameraCommand::SetRunning(running));
        }
        if running && self.camera_command.is_none() {
            self.start_camera();
        }
        if !running {
            self.camera_texture = None;
        }
    }
    fn select_camera(&mut self, index: usize) {
        self.camera_index = index;
        if let Some(command) = &self.camera_command {
            let _ = command.send(CameraCommand::SetCamera(index));
        }
        self.say(format!("{} selected", self.selected_camera_name()));
    }
    fn poll_camera(&mut self, ctx: &egui::Context) {
        if let Some(frames) = &self.camera_frames {
            let mut newest = None;
            while let Ok(frame) = frames.try_recv() {
                newest = Some(frame);
            }
            if let Some(frame) = newest {
                for code in frame.codes {
                    self.record_camera_decoded(code);
                }
                let image = egui::ColorImage::from_rgb([frame.width, frame.height], &frame.pixels);
                if let Some(texture) = &mut self.camera_texture {
                    texture.set(image, egui::TextureOptions::LINEAR);
                } else {
                    self.camera_texture =
                        Some(ctx.load_texture("camera-feed", image, egui::TextureOptions::LINEAR));
                }
            }
        }
    }
    fn icon(ui: &mut egui::Ui, name: &str, color: Color32) {
        let (r, _) = ui.allocate_exact_size(Vec2::splat(20.), Sense::hover());
        let p = ui.painter();
        let s = Stroke::new(1.8, color);
        let x = r.left();
        let y = r.top();
        let l = |a: (f32, f32), b: (f32, f32)| {
            p.line_segment(
                [Pos2::new(x + a.0, y + a.1), Pos2::new(x + b.0, y + b.1)],
                s,
            )
        };
        match name {
            "camera" | "camera-off" => {
                p.rect_stroke(
                    Rect::from_min_size(Pos2::new(x + 2., y + 6.), Vec2::new(16., 11.)),
                    2.,
                    s,
                    StrokeKind::Inside,
                );
                p.circle_stroke(Pos2::new(x + 10., y + 12.), 3., s);
                l((5., 6.), (8., 3.));
                if name == "camera-off" {
                    l((2., 2.), (18., 18.));
                }
            }
            "switch-camera" => {
                l((2., 7.), (17., 7.));
                l((14., 4.), (17., 7.));
                l((14., 10.), (17., 7.));
                l((18., 13.), (3., 13.));
                l((6., 10.), (3., 13.));
                l((6., 16.), (3., 13.));
            }
            "arrow-up" => {
                l((10., 17.), (10., 3.));
                l((5., 8.), (10., 3.));
                l((15., 8.), (10., 3.));
            }
            "arrow-down" => {
                l((10., 3.), (10., 17.));
                l((5., 12.), (10., 17.));
                l((15., 12.), (10., 17.));
            }
            "lightbulb" | "lightbulb-off" => {
                p.circle_stroke(Pos2::new(x + 10., y + 8.), 4., s);
                l((7., 13.), (13., 13.));
                l((8., 16.), (12., 16.));
                l((10., 1.), (10., 3.));
                if name == "lightbulb-off" {
                    l((2., 2.), (18., 18.));
                }
            }
            "file-scan" => {
                p.rect_stroke(
                    Rect::from_min_size(Pos2::new(x + 2., y + 2.), Vec2::splat(16.)),
                    2.,
                    s,
                    StrokeKind::Inside,
                );
                l((5., 8.), (14., 8.));
                l((5., 12.), (11., 12.));
                l((14., 2.), (18., 6.));
            }
            "scan-square" => {
                l((7., 2.), (3., 2.));
                l((3., 2.), (3., 6.));
                l((13., 2.), (17., 2.));
                l((17., 2.), (17., 6.));
                l((3., 14.), (3., 18.));
                l((3., 18.), (7., 18.));
                l((17., 14.), (17., 18.));
                l((17., 18.), (13., 18.));
            }
            "wifi" => {
                p.add(egui::Shape::line(
                    vec![
                        Pos2::new(x + 3., y + 8.),
                        Pos2::new(x + 5., y + 6.),
                        Pos2::new(x + 8., y + 5.),
                        Pos2::new(x + 10., y + 5.),
                        Pos2::new(x + 12., y + 5.),
                        Pos2::new(x + 15., y + 6.),
                        Pos2::new(x + 17., y + 8.),
                    ],
                    s,
                ));
                p.add(egui::Shape::line(
                    vec![
                        Pos2::new(x + 6., y + 11.),
                        Pos2::new(x + 8., y + 9.),
                        Pos2::new(x + 10., y + 9.),
                        Pos2::new(x + 12., y + 9.),
                        Pos2::new(x + 14., y + 11.),
                    ],
                    s,
                ));
                p.circle_filled(Pos2::new(x + 10., y + 15.), 1.5, color);
            }
            "contact" | "book-user" => {
                if name == "book-user" {
                    p.rect_stroke(
                        Rect::from_min_size(Pos2::new(x + 3., y + 2.), Vec2::new(14., 16.)),
                        2.,
                        s,
                        StrokeKind::Inside,
                    );
                    p.circle_stroke(Pos2::new(x + 10., y + 7.), 2.5, s);
                    l((6., 14.), (14., 14.));
                } else {
                    p.circle_stroke(Pos2::new(x + 10., y + 6.), 3., s);
                    p.rect_stroke(
                        Rect::from_min_size(Pos2::new(x + 4., y + 10.), Vec2::new(12., 8.)),
                        3.,
                        s,
                        StrokeKind::Inside,
                    );
                }
            }
            "user" => {
                p.circle_stroke(Pos2::new(x + 10., y + 6.), 3., s);
                l((4., 18.), (4., 16.));
                l((4., 16.), (7., 13.));
                l((7., 13.), (13., 13.));
                l((13., 13.), (16., 16.));
                l((16., 16.), (16., 18.));
            }
            "mail" => {
                p.rect_stroke(
                    Rect::from_min_size(Pos2::new(x + 2., y + 4.), Vec2::new(16., 12.)),
                    2.,
                    s,
                    StrokeKind::Inside,
                );
                l((3., 5.), (10., 11.));
                l((17., 5.), (10., 11.));
            }
            "message-circle" => {
                p.circle_stroke(Pos2::new(x + 9., y + 9.), 7., s);
                l((5., 15.), (3., 18.));
                l((3., 18.), (8., 16.));
            }
            "wallet" | "wallet-minimal" => {
                p.rect_stroke(
                    Rect::from_min_size(Pos2::new(x + 2., y + 5.), Vec2::new(16., 12.)),
                    2.,
                    s,
                    StrokeKind::Inside,
                );
                l((13., 10.), (18., 10.));
                p.circle_filled(Pos2::new(x + 15., y + 10.), 1., color);
            }
            "globe" => {
                p.circle_stroke(Pos2::new(x + 10., y + 10.), 8., s);
                l((7., 3.), (7., 17.));
                l((13., 3.), (13., 17.));
                l((2., 10.), (18., 10.));
            }
            "calendar" => {
                p.rect_stroke(
                    Rect::from_min_size(Pos2::new(x + 3., y + 4.), Vec2::new(14., 14.)),
                    2.,
                    s,
                    StrokeKind::Inside,
                );
                l((3., 8.), (17., 8.));
                l((7., 2.), (7., 6.));
                l((13., 2.), (13., 6.));
            }
            "smartphone" => {
                p.rect_stroke(
                    Rect::from_min_size(Pos2::new(x + 5., y + 2.), Vec2::new(10., 16.)),
                    2.,
                    s,
                    StrokeKind::Inside,
                );
                p.circle_filled(Pos2::new(x + 10., y + 15.), 1., color);
            }
            "home" => {
                l((3., 9.), (10., 3.));
                l((10., 3.), (17., 9.));
                l((5., 8.), (5., 17.));
                l((5., 17.), (15., 17.));
                l((15., 17.), (15., 8.));
                l((8., 17.), (8., 11.));
                l((8., 11.), (12., 11.));
                l((12., 11.), (12., 17.));
            }
            "key" => {
                p.circle_stroke(Pos2::new(x + 7., y + 8.), 4., s);
                l((10., 11.), (17., 18.));
                l((14., 15.), (17., 12.));
            }
            "package" => {
                l((3., 7.), (10., 3.));
                l((10., 3.), (17., 7.));
                l((3., 7.), (3., 16.));
                l((3., 16.), (10., 19.));
                l((10., 19.), (17., 16.));
                l((17., 16.), (17., 7.));
                l((3., 7.), (10., 11.));
                l((10., 11.), (17., 7.));
                l((10., 11.), (10., 19.));
            }
            "link" => {
                p.circle_stroke(Pos2::new(x + 7., y + 8.), 4., s);
                p.circle_stroke(Pos2::new(x + 13., y + 12.), 4., s);
                l((9., 10.), (11., 10.));
            }
            "copy" => {
                p.rect_stroke(
                    Rect::from_min_size(Pos2::new(x + 3., y + 5.), Vec2::new(12., 12.)),
                    2.,
                    s,
                    StrokeKind::Inside,
                );
                p.rect_stroke(
                    Rect::from_min_size(Pos2::new(x + 6., y + 2.), Vec2::new(11., 12.)),
                    2.,
                    s,
                    StrokeKind::Inside,
                );
            }
            "download" => {
                l((10., 2.), (10., 14.));
                l((6., 10.), (10., 14.));
                l((14., 10.), (10., 14.));
                l((3., 18.), (17., 18.));
            }
            "trash" => {
                p.rect_stroke(
                    Rect::from_min_size(Pos2::new(x + 5., y + 6.), Vec2::new(10., 12.)),
                    1.,
                    s,
                    StrokeKind::Inside,
                );
                l((4., 4.), (16., 4.));
                l((8., 2.), (12., 2.));
            }
            "menu" => {
                l((3., 5.), (17., 5.));
                l((3., 10.), (17., 10.));
                l((3., 15.), (17., 15.));
            }
            "info" => {
                p.circle_stroke(Pos2::new(x + 10., y + 10.), 8., s);
                p.circle_filled(Pos2::new(x + 10., y + 6.), 1.2, color);
                l((10., 9.), (10., 15.));
            }
            "refresh" => {
                p.circle_stroke(Pos2::new(x + 10., y + 10.), 7., s);
                l((15., 3.), (18., 3.));
                l((18., 3.), (17., 6.));
                l((5., 17.), (2., 17.));
                l((2., 17.), (3., 14.));
            }
            "audio-waveform" => {
                p.add(egui::Shape::line(
                    vec![
                        Pos2::new(x + 2., y + 11.),
                        Pos2::new(x + 4., y + 8.),
                        Pos2::new(x + 6., y + 14.),
                        Pos2::new(x + 8., y + 5.),
                        Pos2::new(x + 10., y + 16.),
                        Pos2::new(x + 12., y + 7.),
                        Pos2::new(x + 14., y + 13.),
                        Pos2::new(x + 16., y + 9.),
                        Pos2::new(x + 18., y + 11.),
                    ],
                    s,
                ));
            }
            "box" => {
                l((3., 7.), (10., 3.));
                l((10., 3.), (17., 7.));
                l((3., 7.), (3., 16.));
                l((3., 16.), (10., 19.));
                l((10., 19.), (17., 16.));
                l((17., 16.), (17., 7.));
                l((3., 7.), (10., 11.));
                l((10., 11.), (17., 7.));
                l((10., 11.), (10., 19.));
            }
            "external-link" => {
                l((3., 17.), (17., 3.));
                l((11., 3.), (17., 3.));
                l((17., 3.), (17., 9.));
                p.rect_stroke(
                    Rect::from_min_size(Pos2::new(x + 3., y + 7.), Vec2::new(10., 10.)),
                    1.,
                    s,
                    StrokeKind::Inside,
                );
            }
            "volume" | "volume-off" => {
                p.rect_filled(
                    Rect::from_min_size(Pos2::new(x + 3., y + 8.), Vec2::new(4., 5.)),
                    0.,
                    color,
                );
                l((7., 8.), (12., 4.));
                l((7., 13.), (12., 17.));
                l((12., 4.), (12., 17.));
                if name == "volume-off" {
                    l((3., 3.), (17., 17.));
                } else {
                    p.add(egui::Shape::line(
                        vec![
                            Pos2::new(x + 14., y + 7.),
                            Pos2::new(x + 16., y + 10.),
                            Pos2::new(x + 14., y + 13.),
                        ],
                        s,
                    ));
                }
            }
            "eye" | "eye-off" => {
                p.circle_stroke(Pos2::new(x + 10., y + 10.), 7., s);
                p.circle_filled(Pos2::new(x + 10., y + 10.), 2., color);
                if name == "eye-off" {
                    l((2., 2.), (18., 18.));
                }
            }
            _ => {}
        }
    }
    fn icon_button(ui: &mut egui::Ui, name: &str, enabled: bool, tooltip: &str) -> bool {
        let response = ui.add_enabled(
            enabled,
            egui::Button::new(egui::RichText::new(""))
                .min_size(Vec2::splat(40.))
                .fill(Color32::from_rgb(28, 29, 35))
                .corner_radius(CornerRadius::same(20)),
        );
        let r = Rect::from_center_size(response.rect.center(), Vec2::splat(20.));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
        Self::icon(
            &mut child,
            name,
            if enabled {
                Color32::from_rgb(225, 227, 234)
            } else {
                Color32::from_rgb(90, 91, 98)
            },
        );
        if ui.rect_contains_pointer(response.rect) {
            ui.ctx().set_cursor_icon(if enabled {
                egui::CursorIcon::PointingHand
            } else {
                egui::CursorIcon::NotAllowed
            });
        }
        response.on_hover_text(tooltip).clicked()
    }
    fn text_action_sized(
        ui: &mut egui::Ui,
        icon: &str,
        label: &str,
        enabled: bool,
        width: f32,
    ) -> bool {
        let response = ui.add_enabled(
            enabled,
            egui::Button::new(egui::RichText::new("").size(12.))
                .min_size(Vec2::new(width, 34.))
                .fill(Color32::from_rgb(31, 32, 39))
                .corner_radius(CornerRadius::same(7)),
        );
        let r = Rect::from_min_size(
            Pos2::new(response.rect.left() + 8., response.rect.center().y - 10.),
            Vec2::splat(20.),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
        Self::icon(
            &mut child,
            icon,
            if enabled {
                Color32::from_rgb(210, 212, 221)
            } else {
                Color32::from_rgb(90, 91, 98)
            },
        );
        ui.painter().text(
            Pos2::new(response.rect.left() + 32., response.rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::proportional(12.),
            if enabled {
                Color32::from_rgb(225, 227, 234)
            } else {
                Color32::from_rgb(90, 91, 98)
            },
        );
        if ui.rect_contains_pointer(response.rect) {
            ui.ctx().set_cursor_icon(if enabled {
                egui::CursorIcon::PointingHand
            } else {
                egui::CursorIcon::NotAllowed
            });
        }
        response.clicked()
    }
}

impl eframe::App for ScanBarcode {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("camera_on", self.camera_on.to_string());
        storage.set_string("camera_index", self.camera_index.to_string());
        storage.set_string("mirror_camera", self.mirror_camera.to_string());
        storage.set_string("show_preview", self.show_preview.to_string());
        storage.set_string("mute_sound", self.mute_sound.to_string());
        storage.set_string("auto_updates", self.auto_updates.to_string());
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_visuals(egui::Visuals::dark());
        if ctx.input(|i| i.viewport().close_requested())
            && !self.scanned_items.is_empty()
            && !self.close_confirmed
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }
        let move_shortcut = ctx.input(move_shortcut_direction);
        let mut move_up = move_shortcut == Some(-1);
        let mut move_down = move_shortcut == Some(1);
        if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            self.camera_on = !self.camera_on;
            self.set_camera_running(self.camera_on);
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::L)) {
            self.torch_on = !self.torch_on;
        }
        if ctx.input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::C)) {
            self.copy();
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O)) {
            load_image(self);
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)) {
            self.download();
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::K)) {
            self.request_clear();
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::M)) {
            self.mute_sound = !self.mute_sound;
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::R)) {
            self.mirror_camera = !self.mirror_camera;
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::P)) {
            self.show_preview = !self.show_preview;
        }
        if ctx.input(|i| {
            i.modifiers.command && i.key_pressed(egui::Key::Tab) && self.camera_count > 1
        }) {
            self.select_camera((self.camera_index + 1) % self.camera_count);
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter)) {
            self.execute();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.menu_open = false;
            self.confirm_close = false;
        }
        #[cfg(target_os = "macos")]
        for event in muda::MenuEvent::receiver().try_iter() {
            match event.id.as_ref() {
                "scanbarcode.about" => self.about_open = true,
                "scanbarcode.load" => load_image(self),
                "scanbarcode.copy" => self.copy(),
                "scanbarcode.save" => self.download(),
                "scanbarcode.clear" => self.request_clear(),
                "scanbarcode.move_up" => move_up = true,
                "scanbarcode.move_down" => move_down = true,
                "scanbarcode.mirror" => self.mirror_camera = !self.mirror_camera,
                "scanbarcode.camera" => {
                    self.camera_on = !self.camera_on;
                    self.set_camera_running(self.camera_on);
                }
                "scanbarcode.preview" => self.show_preview = !self.show_preview,
                "scanbarcode.light" => self.torch_on = !self.torch_on,
                "scanbarcode.mute" => self.mute_sound = !self.mute_sound,
                "scanbarcode.switch_camera" if self.camera_count > 1 => {
                    self.select_camera((self.camera_index + 1) % self.camera_count)
                }
                "scanbarcode.action" => self.execute(),
                "scanbarcode.quit" => self.request_close(&ctx),
                _ => {}
            }
        }
        if move_up {
            self.move_selected_scan(-1);
        }
        if move_down {
            self.move_selected_scan(1);
        }
        self.poll_camera(&ctx);
        self.poll_updates();
        render(self, ui);
        ctx.request_repaint_after(std::time::Duration::from_millis(32));
    }
}

fn move_shortcut_direction(input: &egui::InputState) -> Option<isize> {
    let modifiers = input.modifiers;
    if !modifiers.shift || modifiers.command || modifiers.ctrl || modifiers.alt {
        return None;
    }
    if input.key_pressed(egui::Key::ArrowUp) {
        Some(-1)
    } else if input.key_pressed(egui::Key::ArrowDown) {
        Some(1)
    } else {
        None
    }
}

fn render(app: &mut ScanBarcode, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let style = ui.style_mut();
    style.spacing.item_spacing = Vec2::new(8., 8.);
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(26, 27, 33);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(52, 53, 62);
    let full = ui.available_rect_before_wrap();
    let panel_w = (full.width() * 0.27).clamp(290., 360.);
    let camera = Rect::from_min_size(full.min, Vec2::new(full.width() - panel_w, full.height()));
    let panel = Rect::from_min_size(
        Pos2::new(camera.right(), full.top()),
        Vec2::new(panel_w, full.height()),
    );
    ui.painter()
        .rect_filled(camera, 0., Color32::from_rgb(3, 4, 6));
    if app.camera_on && app.show_preview {
        if let Some(texture) = &app.camera_texture {
            let size = texture.size();
            let source_aspect = size[0] as f32 / size[1].max(1) as f32;
            let target_aspect = camera.width() / camera.height().max(1.);
            let draw_size = if source_aspect > target_aspect {
                Vec2::new(camera.width(), camera.width() / source_aspect)
            } else {
                Vec2::new(camera.height() * source_aspect, camera.height())
            };
            let draw_rect = Rect::from_center_size(camera.center(), draw_size);
            let uv = if app.mirror_camera {
                egui::Rect {
                    min: Pos2::new(1., 0.),
                    max: Pos2::new(0., 1.),
                }
            } else {
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.))
            };
            ui.painter()
                .image(texture.id(), draw_rect, uv, Color32::WHITE);
        }
    }
    for y in (camera.top() as i32..camera.bottom() as i32).step_by(4) {
        ui.painter().line_segment(
            [
                Pos2::new(camera.left(), y as f32),
                Pos2::new(camera.right(), y as f32),
            ],
            Stroke::new(1., Color32::from_rgba_unmultiplied(255, 255, 255, 2)),
        );
    }
    let menu_rect = Rect::from_min_size(
        Pos2::new(camera.left() + 18., camera.top() + 18.),
        Vec2::splat(40.),
    );
    let menu_response = ui.put(
        menu_rect,
        egui::Button::new(egui::RichText::new(""))
            .fill(Color32::from_rgba_unmultiplied(28, 29, 35, 215))
            .corner_radius(CornerRadius::same(20)),
    );
    let mut menu_icon = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(
        menu_response.rect.center(),
        Vec2::splat(20.),
    )));
    ScanBarcode::icon(&mut menu_icon, "menu", Color32::from_rgb(180, 182, 190));
    if menu_response.on_hover_text("ScanBarcode menu").clicked() {
        app.menu_open = !app.menu_open;
    }
    if app.menu_open && ctx.input(|input| input.pointer.any_click()) {
        let popup_bounds = Rect::from_min_size(
            Pos2::new(camera.left() + 18., camera.top() + 66.),
            Vec2::new(245., 560.),
        );
        let pointer = ctx.input(|input| input.pointer.interact_pos());
        if pointer
            .map(|point| !popup_bounds.contains(point) && !menu_rect.contains(point))
            .unwrap_or(false)
        {
            app.menu_open = false;
        }
    }
    if app.menu_open {
        egui::Area::new(egui::Id::unique("actions-menu"))
            .fixed_pos(Pos2::new(camera.left() + 18., camera.top() + 66.))
            .show(&ctx, |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgb(31, 31, 33))
                    .stroke(Stroke::new(1., Color32::from_rgb(76, 78, 86)))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(8, 8))
                    .show(ui, |ui| {
                        ui.style_mut().spacing.button_padding = Vec2::new(10., 7.);
                        ui.style_mut().visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
                        ui.style_mut().visuals.widgets.hovered.bg_fill =
                            Color32::from_rgb(46, 48, 57);
                        ui.style_mut().visuals.widgets.active.bg_fill =
                            Color32::from_rgb(58, 61, 72);
                        ui.set_width(245.);
                        ui.label(egui::RichText::new("ScanBarcode").strong());
                        ui.separator();
                        let mod_key = if cfg!(target_os = "macos") {
                            "⌘"
                        } else {
                            "Ctrl"
                        };
                        if menu_item(
                            ui,
                            "file-scan",
                            "Load image…",
                            &format!("{mod_key}O"),
                            true,
                            false,
                        ) {
                            app.menu_open = false;
                            load_image(app);
                        }
                        if menu_item(
                            ui,
                            "copy",
                            "Copy results",
                            &format!("{mod_key} ⇧ C"),
                            !app.scanned_text.is_empty(),
                            false,
                        ) {
                            app.menu_open = false;
                            app.copy();
                        }
                        if menu_item(
                            ui,
                            "download",
                            "Save results…",
                            &format!("{mod_key}S"),
                            !app.scanned_text.is_empty(),
                            false,
                        ) {
                            app.menu_open = false;
                            app.download();
                        }
                        if menu_item(
                            ui,
                            "trash",
                            "Clear results",
                            &format!("{mod_key}K"),
                            !app.scanned_text.is_empty(),
                            false,
                        ) {
                            app.menu_open = false;
                            app.request_clear();
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            "arrow-up",
                            "Move selected up",
                            "⇧↑",
                            app.selected_scan.is_some_and(|index| index > 0),
                            false,
                        ) {
                            app.move_selected_scan(-1);
                            app.menu_open = false;
                        }
                        if menu_item(
                            ui,
                            "arrow-down",
                            "Move selected down",
                            "⇧↓",
                            app.selected_scan
                                .is_some_and(|index| index + 1 < app.scanned_items.len()),
                            false,
                        ) {
                            app.move_selected_scan(1);
                            app.menu_open = false;
                        }
                        if menu_item(
                            ui,
                            "switch-camera",
                            "Mirror camera preview",
                            &format!("{mod_key}R"),
                            true,
                            false,
                        ) {
                            app.mirror_camera = !app.mirror_camera;
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            if app.camera_on {
                                "camera"
                            } else {
                                "camera-off"
                            },
                            "Camera on / off",
                            "Space",
                            true,
                            false,
                        ) {
                            app.camera_on = !app.camera_on;
                            app.set_camera_running(app.camera_on);
                            app.menu_open = false;
                        }
                        if menu_item(
                            ui,
                            if app.mute_sound {
                                "volume-off"
                            } else {
                                "volume"
                            },
                            "Mute scan sounds",
                            &format!("{mod_key}M"),
                            true,
                            false,
                        ) {
                            app.mute_sound = !app.mute_sound;
                            app.menu_open = false;
                        }
                        if menu_item(
                            ui,
                            if app.show_preview { "eye" } else { "eye-off" },
                            "Show camera preview",
                            &format!("{mod_key}P"),
                            true,
                            false,
                        ) {
                            app.show_preview = !app.show_preview;
                        }
                        if menu_item(
                            ui,
                            if app.torch_on {
                                "lightbulb"
                            } else {
                                "lightbulb-off"
                            },
                            "Toggle light",
                            &format!("{mod_key}L"),
                            true,
                            false,
                        ) {
                            app.torch_on = !app.torch_on;
                            app.menu_open = false;
                        }
                        if menu_item(
                            ui,
                            "switch-camera",
                            "Switch camera",
                            &format!("{mod_key}Tab"),
                            app.camera_count > 1,
                            false,
                        ) {
                            app.select_camera((app.camera_index + 1) % app.camera_count);
                            app.menu_open = false;
                        }
                        ui.separator();
                        if menu_item(
                            ui,
                            action_icon(app.last_action.as_deref()),
                            app.last_action.as_deref().unwrap_or("Open detected action"),
                            &format!("{mod_key}Enter"),
                            app.last_deeplink.is_some(),
                            false,
                        ) {
                            app.menu_open = false;
                            app.execute();
                        }
                        if menu_item(ui, "info", "About", "", true, false) {
                            app.about_open = true;
                            app.menu_open = false;
                        }
                    });
            });
    }
    ui.painter()
        .rect_filled(panel, 0., Color32::from_rgb(14, 15, 19));
    ui.painter().line_segment(
        [
            Pos2::new(panel.left(), panel.top()),
            Pos2::new(panel.left(), panel.bottom()),
        ],
        Stroke::new(1., Color32::from_rgb(42, 43, 50)),
    );
    let mut controls = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
        Pos2::new(camera.right() - 252., camera.top() + 18.),
        Vec2::new(244., 44.),
    )));
    controls.horizontal(|ui| {
        if ScanBarcode::icon_button(
            ui,
            if app.camera_on {
                "camera"
            } else {
                "camera-off"
            },
            true,
            "Camera on/off · Space",
        ) {
            app.camera_on = !app.camera_on;
            app.set_camera_running(app.camera_on);
        }
        if ScanBarcode::icon_button(
            ui,
            if app.show_preview { "eye" } else { "eye-off" },
            true,
            "Show or hide preview",
        ) {
            app.show_preview = !app.show_preview;
        }
        if ScanBarcode::icon_button(
            ui,
            "switch-camera",
            app.camera_count > 1,
            if app.camera_count > 1 {
                if cfg!(target_os = "macos") {
                    "Switch camera · ⌘Tab"
                } else {
                    "Switch camera · Ctrl+Tab"
                }
            } else {
                "Only one camera detected"
            },
        ) {
            let next = (app.camera_index + 1) % app.camera_count;
            app.select_camera(next);
        }
        if ScanBarcode::icon_button(
            ui,
            if app.torch_on {
                "lightbulb"
            } else {
                "lightbulb-off"
            },
            true,
            if cfg!(target_os = "macos") {
                "Light on/off · ⌘L"
            } else {
                "Light on/off · Ctrl+L"
            },
        ) {
            app.torch_on = !app.torch_on;
        }
        if ScanBarcode::icon_button(ui, "file-scan", true, "Load from image · ⌘O / Ctrl+O") {
            load_image(app);
        }
    });
    let q = Rect::from_center_size(
        camera.center(),
        Vec2::splat(camera.height().min(camera.width() * 0.62).min(430.)),
    );
    let s = Stroke::new(4., Color32::from_rgb(204, 206, 214));
    for (a, b) in [
        ((q.left(), q.top() + 30.), (q.left(), q.top())),
        ((q.left(), q.top()), (q.left() + 30., q.top())),
        ((q.right() - 30., q.top()), (q.right(), q.top())),
        ((q.right(), q.top()), (q.right(), q.top() + 30.)),
        ((q.left(), q.bottom() - 30.), (q.left(), q.bottom())),
        ((q.left(), q.bottom()), (q.left() + 30., q.bottom())),
        ((q.right() - 30., q.bottom()), (q.right(), q.bottom())),
        ((q.right(), q.bottom()), (q.right(), q.bottom() - 30.)),
    ] {
        ui.painter()
            .line_segment([Pos2::new(a.0, a.1), Pos2::new(b.0, b.1)], s);
    }
    if !app.camera_on {
        ui.painter().text(
            q.center(),
            egui::Align2::CENTER_CENTER,
            "CAMERA OFF",
            FontId::proportional(14.),
            Color32::from_rgb(135, 137, 145),
        );
    } else {
        ui.painter().text(
            Pos2::new(q.center().x, q.bottom() + 28.),
            egui::Align2::CENTER_CENTER,
            format!("{}  ·  Point at a code", app.selected_camera_name()),
            FontId::proportional(13.),
            Color32::from_rgb(145, 147, 155),
        );
    }
    right_panel(app, ui, panel);
    if app.confirm_clear {
        egui::Window::new("Clear results")
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .fixed_size(Vec2::new(390., 176.))
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(&ctx, |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgb(35, 36, 42))
                    .inner_margin(egui::Margin::symmetric(18, 14))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (icon_rect, _) =
                                ui.allocate_exact_size(Vec2::splat(24.), Sense::hover());
                            let mut icon_ui =
                                ui.new_child(egui::UiBuilder::new().max_rect(icon_rect));
                            ScanBarcode::icon(
                                &mut icon_ui,
                                "trash",
                                Color32::from_rgb(242, 161, 161),
                            );
                            ui.add_space(8.);
                            ui.label(egui::RichText::new("Clear results").strong().size(16.));
                        });
                        ui.add_space(14.);
                        ui.label("Remove all scanned items from this session?");
                        ui.add_space(18.);
                        ui.horizontal(|ui| {
                            let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.;
                            if ui
                                .add_sized([width, 40.], egui::Button::new("Cancel"))
                                .clicked()
                            {
                                app.confirm_clear = false;
                            }
                            if ui
                                .add_sized(
                                    [width, 40.],
                                    egui::Button::new("Clear all")
                                        .fill(Color32::from_rgb(138, 61, 67))
                                        .corner_radius(CornerRadius::same(7)),
                                )
                                .clicked()
                            {
                                app.confirm_clear = false;
                                app.clear();
                            }
                        });
                    });
            });
    }
    if app.confirm_close {
        egui::Window::new("Close ScanBarcode")
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .fixed_size(Vec2::new(390., 176.))
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(&ctx, |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgb(35, 36, 42))
                    .inner_margin(egui::Margin::symmetric(18, 14))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("Close ScanBarcode?").strong().size(16.));
                        ui.add_space(14.);
                        ui.label(format!(
                            "{} scanned item{} will be lost when you close.",
                            app.scanned_items.len(),
                            if app.scanned_items.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        ));
                        ui.add_space(18.);
                        ui.horizontal(|ui| {
                            let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.;
                            if ui
                                .add_sized([width, 40.], egui::Button::new("Cancel"))
                                .clicked()
                            {
                                app.confirm_close = false;
                            }
                            if ui
                                .add_sized(
                                    [width, 40.],
                                    egui::Button::new("Close anyway")
                                        .fill(Color32::from_rgb(138, 61, 67))
                                        .corner_radius(CornerRadius::same(7)),
                                )
                                .clicked()
                            {
                                app.close_confirmed = true;
                                app.confirm_close = false;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        });
                    });
            });
    }
    if app.about_open {
        egui::Window::new("About ScanBarcode")
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .fixed_size(Vec2::new(640., 700.))
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(&ctx, |ui| {
                let content_width = ui.available_width();
                egui::Frame::new()
                    .fill(Color32::from_rgb(43, 44, 50))
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(content_width);
                        if cfg!(target_os = "macos") {
                            ui.horizontal(|ui| {
                                if ui
                                    .add(egui::Button::new(egui::RichText::new("×").size(20.)))
                                    .on_hover_text("Close")
                                    .clicked()
                                {
                                    app.about_open = false;
                                }
                                ui.add_space(8.);
                                ui.label(egui::RichText::new("About ScanBarcode").strong());
                            });
                        } else {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("About ScanBarcode").strong());
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .add(egui::Button::new(
                                                egui::RichText::new("×").size(20.),
                                            ))
                                            .on_hover_text("Close")
                                            .clicked()
                                        {
                                            app.about_open = false;
                                        }
                                    },
                                );
                            });
                        }
                    });
                ui.separator();
                ui.add_space(10.);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("ScanBarcode").size(26.).strong());
                    ui.add_space(8.);
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                        ui.label(
                            egui::RichText::new(format!("Version {APP_VERSION}"))
                                .color(Color32::from_rgb(170, 201, 235)),
                        );
                    });
                });
                ui.add_space(8.);
                ui.horizontal(|ui| {
                    let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.;
                    if ScanBarcode::text_action_sized(
                        ui,
                        "external-link",
                        "Report bug",
                        true,
                        width,
                    ) {
                        open_external(BUG_REPORT_URL);
                    }
                    if ScanBarcode::text_action_sized(
                        ui,
                        "external-link",
                        "Request feature",
                        true,
                        width,
                    ) {
                        open_external(FEATURE_REQUEST_URL);
                    }
                });
                ui.add_space(18.);
                ui.label(egui::RichText::new("License").strong());
                ui.add_space(8.);
                egui::Frame::new()
                    .fill(Color32::from_rgb(24, 25, 29))
                    .stroke(Stroke::new(1., Color32::from_rgb(57, 59, 67)))
                    .corner_radius(CornerRadius::same(7))
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        let mut license = CORE_LICENSE_TEXT.to_string();
                        ui.add_sized(
                            Vec2::new(ui.available_width(), 280.),
                            egui::TextEdit::multiline(&mut license)
                                .desired_width(f32::INFINITY)
                                .interactive(false),
                        );
                    });
                ui.add_space(16.);
                ui.separator();
                egui::Frame::new()
                    .fill(Color32::from_rgb(31, 32, 38))
                    .stroke(Stroke::new(1., Color32::from_rgb(57, 59, 67)))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_width(content_width);
                        ui.horizontal(|ui| {
                            let (icon_rect, _) =
                                ui.allocate_exact_size(Vec2::splat(22.), Sense::hover());
                            let mut icon_ui =
                                ui.new_child(egui::UiBuilder::new().max_rect(icon_rect));
                            ScanBarcode::icon(
                                &mut icon_ui,
                                "refresh",
                                Color32::from_rgb(170, 201, 235),
                            );
                            ui.add_space(4.);
                            ui.label(egui::RichText::new("Updates").strong().size(15.));
                        });
                        ui.add_space(6.);
                        ui.checkbox(&mut app.auto_updates, "Automatically check for updates");
                        ui.add_space(4.);
                        let status_color = if app.update_status.contains("latest") {
                            Color32::from_rgb(145, 220, 176)
                        } else {
                            Color32::from_rgb(180, 182, 192)
                        };
                        ui.label(egui::RichText::new(&app.update_status).color(status_color));
                        ui.add_space(8.);
                        ui.horizontal(|ui| {
                            let update_available = app
                                .update_version
                                .as_deref()
                                .is_some_and(|version| is_newer_version(version, APP_VERSION))
                                && app.update_phase != UpdatePhase::Checking;
                            let width = if update_available {
                                (ui.available_width() - ui.spacing().item_spacing.x) / 2.
                            } else {
                                ui.available_width()
                            };
                            if ui
                                .add_sized([width, 38.], egui::Button::new("Check now"))
                                .clicked()
                            {
                                app.start_update_check();
                            }
                            if update_available {
                                if ui
                                    .add_sized(
                                        [width, 38.],
                                        egui::Button::new("Update now")
                                            .fill(Color32::from_rgb(60, 108, 162))
                                            .corner_radius(CornerRadius::same(7)),
                                    )
                                    .clicked()
                                {
                                    app.about_open = false;
                                    app.update_prompt = true;
                                }
                            }
                        });
                    });
            });
    }
    if app.update_prompt {
        egui::Window::new("Software update")
            .collapsible(false)
            .resizable(false)
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(&ctx, |ui| {
                egui::Frame::new()
                    .fill(Color32::from_rgb(28, 32, 41))
                    .stroke(Stroke::new(1., Color32::from_rgb(69, 88, 112)))
                    .corner_radius(CornerRadius::same(14))
                    .inner_margin(egui::Margin::symmetric(24, 22))
                    .show(ui, |ui| {
                        ui.set_width(430.);
                        ui.horizontal(|ui| {
                            let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(32.), Sense::hover());
                            let mut icon_ui = ui.new_child(egui::UiBuilder::new().max_rect(icon_rect));
                            ScanBarcode::icon(&mut icon_ui, "refresh", Color32::from_rgb(124, 183, 248));
                            ui.add_space(8.);
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new("ScanBarcode update").strong().size(20.));
                                ui.label(egui::RichText::new(format!(
                                    "Version {APP_VERSION}  →  {}",
                                    app.update_version.as_deref().unwrap_or("new version")
                                )).color(Color32::from_rgb(157, 172, 194)));
                            });
                        });
                        ui.add_space(20.);
                        match app.update_phase {
                            UpdatePhase::Available => {
                                ui.label("A new version is available for this computer.");
                                ui.label(egui::RichText::new("Download it now, then choose when to install and relaunch.")
                                    .color(Color32::from_rgb(163, 171, 185)));
                            }
                            UpdatePhase::Downloading | UpdatePhase::Pausing | UpdatePhase::Paused => {
                                ui.label(match app.update_phase {
                                    UpdatePhase::Pausing => "Pausing download",
                                    UpdatePhase::Paused => "Download paused",
                                    _ => "Downloading update",
                                });
                                ui.add_space(8.);
                                if let Some(progress) = app.update_progress {
                                    ui.add(egui::ProgressBar::new(progress.clamp(0., 1.))
                                        .desired_width(ui.available_width())
                                        .show_percentage());
                                } else {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label("Connecting to the download…");
                                    });
                                }
                                ui.label(egui::RichText::new(format_download_progress(
                                    app.update_downloaded,
                                    app.update_total,
                                )).color(Color32::from_rgb(163, 171, 185)));
                                if app.update_phase == UpdatePhase::Paused {
                                    ui.label(egui::RichText::new("You can continue later while the app stays open.")
                                        .color(Color32::from_rgb(163, 171, 185)));
                                }
                            }
                            UpdatePhase::Ready => {
                                ui.label(egui::RichText::new("Download complete")
                                    .color(Color32::from_rgb(145, 220, 176)).strong());
                                ui.label("SHA-256 verified against GitHub. Ready to install and relaunch.");
                            }
                            UpdatePhase::Installing => {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label("Preparing installation and relaunch…");
                                });
                            }
                            UpdatePhase::Error => {
                                ui.label(egui::RichText::new(&app.update_status)
                                    .color(Color32::from_rgb(246, 158, 158)));
                            }
                            _ => { ui.label(&app.update_status); }
                        }
                        if !app.scanned_items.is_empty() && app.update_phase == UpdatePhase::Ready {
                            ui.add_space(8.);
                            ui.label(egui::RichText::new("Save your scan results first; they will close during installation.")
                                .color(Color32::from_rgb(245, 195, 136)));
                        }
                        ui.add_space(22.);
                        ui.separator();
                        ui.add_space(12.);
                        ui.horizontal(|ui| {
                            let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.;
                            if ui.add_sized([width, 42.], egui::Button::new("Later")).clicked() {
                                app.update_prompt = false;
                            }
                                let label = match app.update_phase {
                                    UpdatePhase::Available => "Download update",
                                    UpdatePhase::Ready => "Install and relaunch",
                                    UpdatePhase::Error if app.update_archive.is_some() => "Try installation again",
                                    UpdatePhase::Error => "Try download again",
                                    UpdatePhase::Downloading => "Pause download",
                                    UpdatePhase::Pausing => "Pausing…",
                                    UpdatePhase::Paused => "Continue download",
                                    UpdatePhase::Installing => "Installing…",
                                    _ => "Continue",
                                };
                                let enabled = !matches!(app.update_phase, UpdatePhase::Pausing | UpdatePhase::Installing)
                                    && (app.update_archive.is_some() || (app.update_asset_url.is_some() && app.update_digest.is_some()));
                                if ui.add_enabled_ui(enabled, |ui| ui.add_sized(
                                    [width, 42.],
                                    egui::Button::new(label)
                                        .fill(Color32::from_rgb(60, 108, 162))
                                        .corner_radius(CornerRadius::same(8)),
                                )).inner.clicked() {
                                    if app.update_phase == UpdatePhase::Downloading {
                                        app.pause_update();
                                    } else if app.update_archive.is_some() {
                                        app.install_update();
                                    } else {
                                        app.apply_update();
                                    }
                                }
                        });
                    });
            });
    }
    if let Some(message) = &app.toast {
        egui::Area::new(egui::Id::unique("toast"))
            .fixed_pos(Pos2::new(24., full.bottom() - 42.))
            .show(&ctx, |ui| {
                ui.label(egui::RichText::new(message).color(Color32::from_rgb(159, 238, 198)));
            });
    }
}

fn right_panel(app: &mut ScanBarcode, ui: &mut egui::Ui, rect: Rect) {
    let mut panel = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(22.)));
    panel.vertical(|ui| {
        ui.add_space(12.);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Results")
                    .size(17.)
                    .strong()
                    .color(Color32::from_rgb(239, 240, 244)),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} code{}",
                        app.scanned_items.len(),
                        if app.scanned_items.len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    ))
                    .size(14.)
                    .strong()
                    .color(Color32::from_rgb(130, 132, 142)),
                );
            });
        });
        ui.add_space(12.);
        let height = (ui.available_height() - 124.).max(180.);
        scan_rows(app, ui, height);
        if app.barcode_tx.is_active() || app.barcode_tx.result.is_some() {
            ui.add_space(8.);
            let progress = app.barcode_tx.progress().clamp(0., 1.);
            ui.add(
                egui::ProgressBar::new(progress)
                    .show_percentage()
                    .text(format!(
                        "Animated BarcodeTx · {} frames",
                        app.barcode_tx.received_symbols()
                    )),
            );
            if let Some(result) = &app.barcode_tx.result {
                ui.label(format!(
                    "Complete: {} transaction{} ({} XCB supported)",
                    result.transactions,
                    if result.transactions == 1 { "" } else { "s" },
                    result.supported_xcb
                ));
            }
        }
        ui.add_space(10.);
        let action_width = ((ui.available_width() - 16.) / 3.).max(0.);
        ui.horizontal(|ui| {
            if ScanBarcode::text_action_sized(
                ui,
                "copy",
                "Copy",
                !app.scanned_text.is_empty(),
                action_width,
            ) {
                app.copy();
            }
            if ScanBarcode::text_action_sized(
                ui,
                "download",
                "Save",
                !app.scanned_text.is_empty(),
                action_width,
            ) {
                app.download();
            }
            if ScanBarcode::text_action_sized(
                ui,
                "trash",
                "Clear",
                !app.scanned_text.is_empty(),
                action_width,
            ) {
                app.request_clear();
            }
        });
        ui.add_space(16.);
        let available = app.last_action.is_some() && app.last_deeplink.is_some();
        let label = app
            .last_action
            .clone()
            .unwrap_or_else(|| "No action available".to_string());
        if action_button(
            ui,
            action_icon(app.last_action.as_deref()),
            &label,
            available,
        ) {
            app.execute();
        }
    });
}

fn scan_rows(app: &mut ScanBarcode, ui: &mut egui::Ui, height: f32) {
    let width = ui.available_width();
    let pointer = ui.ctx().pointer_interact_pos();
    let released = ui.input(|input| input.pointer.any_released());
    let mut remove = None;
    let mut select = None;
    let mut drop_target = None;
    let mut dropped_scan = None;
    let mut row_bounds = None;
    ui.allocate_ui_with_layout(
        Vec2::new(width, height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.style_mut().spacing.scroll.floating = false;
            egui::Frame::new()
                .fill(Color32::from_rgb(25, 26, 32))
                .corner_radius(8)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(height)
                        .auto_shrink([false, false])
                        .content_margin(egui::Margin::ZERO)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            if app.scanned_items.is_empty() {
                                ui.add_space(12.);
                                ui.label("Scanned results will appear here and will be cleared when you close the app.");
                            }
                            for (index, item) in app.scanned_items.iter().enumerate() {
                                let (row, row_response) = ui.allocate_exact_size(
                                    Vec2::new(ui.available_width(), 38.),
                                    Sense::click(),
                                );
                                if row_response.clicked() {
                                    select = Some(index);
                                }
                                row_bounds.get_or_insert(row);
                                let hovered_drop = row_response.dnd_hover_payload::<usize>().is_some()
                                    || (app.dragged_scan.is_some()
                                        && pointer.is_some_and(|position| row.contains(position)));
                                if hovered_drop {
                                    drop_target = Some(index);
                                }
                                if let Some(from) = row_response.dnd_release_payload::<usize>() {
                                    dropped_scan = Some((*from, index));
                                }
                                let fill = if app.dragged_scan == Some(index) {
                                    Color32::from_rgb(23, 25, 31)
                                } else if hovered_drop {
                                    Color32::from_rgb(49, 72, 91)
                                } else if app.selected_scan == Some(index) {
                                    Color32::from_rgb(35, 48, 63)
                                } else if index % 2 == 0 {
                                    Color32::from_rgb(31, 33, 40)
                                } else {
                                    Color32::from_rgb(27, 29, 36)
                                };
                                ui.painter().rect_filled(row, 0., fill);
                                if app.selected_scan == Some(index) {
                                    ui.painter().rect_filled(
                                        Rect::from_min_size(row.min, Vec2::new(3., row.height())),
                                        0.,
                                        Color32::from_rgb(113, 190, 235),
                                    );
                                }
                                ui.painter().line_segment(
                                    [row.left_bottom(), row.right_bottom()],
                                    Stroke::new(1., Color32::from_rgb(49, 51, 59)),
                                );
                                if hovered_drop && app.dragged_scan != Some(index) {
                                    ui.painter().line_segment(
                                        [row.left_top(), row.right_top()],
                                        Stroke::new(2., Color32::from_rgb(101, 183, 232)),
                                    );
                                }
                                let handle = Rect::from_min_size(row.min, Vec2::new(27., row.height()));
                                let handle_response = ui.interact(
                                    handle,
                                    ui.scope_id().with(("scan_handle", index)),
                                    Sense::drag(),
                                );
                                handle_response.dnd_set_drag_payload(index);
                                if app.dragged_scan.is_none()
                                    && handle_response.is_pointer_button_down_on()
                                {
                                    app.dragged_scan = Some(index);
                                }
                                if handle_response.dragged() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                                } else if handle_response.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                                }
                                let grip = Color32::from_rgb(138, 145, 157);
                                for y in [-4., 1., 6.] {
                                    for x in [-3., 3.] {
                                        ui.painter().circle_filled(
                                            Pos2::new(handle.center().x + x, handle.center().y + y),
                                            1.3,
                                            grip,
                                        );
                                    }
                                }
                                let delete = Rect::from_center_size(
                                    Pos2::new(row.right() - 17., row.center().y),
                                    Vec2::splat(26.),
                                );
                                let delete_response = ui.interact(
                                    delete,
                                    ui.scope_id().with(("scan_delete", index)),
                                    Sense::click(),
                                ).on_hover_text("Delete row");
                                if delete_response.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if delete_response.clicked() {
                                    remove = Some(index);
                                }
                                ui.painter().text(
                                    delete.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "×",
                                    FontId::proportional(19.),
                                    if delete_response.hovered() {
                                        Color32::from_rgb(245, 132, 132)
                                    } else {
                                        Color32::from_rgb(151, 157, 168)
                                    },
                                );
                                let text_rect = Rect::from_min_max(
                                    Pos2::new(row.left() + 35., row.top()),
                                    Pos2::new(delete.left() - 4., row.bottom()),
                                );
                                ui.painter().with_clip_rect(text_rect).text(
                                    Pos2::new(text_rect.left(), row.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    item.replace('\n', " ↵ "),
                                    FontId::proportional(14.),
                                    Color32::from_rgb(225, 228, 235),
                                );
                                let text_response = ui.interact(
                                    text_rect,
                                    ui.scope_id().with(("scan_text", index)),
                                    Sense::click(),
                                );
                                if text_response.clicked() {
                                    select = Some(index);
                                }
                                if text_response.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                text_response.on_hover_text(item);
                            }
                        });
                });
        },
    );
    if !released {
        if let (Some(index), Some(pointer), Some(bounds)) = (app.dragged_scan, pointer, row_bounds)
        {
            if let Some(item) = app.scanned_items.get(index) {
                let ghost = Rect::from_min_size(
                    Pos2::new(bounds.left(), pointer.y - 19.),
                    Vec2::new(bounds.width(), 38.),
                );
                let painter = ui.ctx().layer_painter(egui::LayerId::new(
                    egui::Order::Tooltip,
                    ui.scope_id().with("scan_drag_preview"),
                ));
                painter.rect_filled(
                    ghost.translate(Vec2::new(2., 3.)),
                    5.,
                    Color32::from_black_alpha(95),
                );
                painter.rect_filled(ghost, 5., Color32::from_rgb(47, 70, 88));
                painter.rect_stroke(
                    ghost,
                    5.,
                    Stroke::new(1., Color32::from_rgb(113, 190, 235)),
                    StrokeKind::Inside,
                );
                for y in [-4., 1., 6.] {
                    for x in [-3., 3.] {
                        painter.circle_filled(
                            Pos2::new(ghost.left() + 13. + x, ghost.center().y + y),
                            1.3,
                            Color32::from_rgb(197, 216, 230),
                        );
                    }
                }
                let text_rect = Rect::from_min_max(
                    Pos2::new(ghost.left() + 35., ghost.top()),
                    Pos2::new(ghost.right() - 36., ghost.bottom()),
                );
                painter.with_clip_rect(text_rect).text(
                    Pos2::new(text_rect.left(), ghost.center().y),
                    egui::Align2::LEFT_CENTER,
                    item.replace('\n', " ↵ "),
                    FontId::proportional(14.),
                    Color32::WHITE,
                );
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
        }
    }
    if let Some(index) = remove {
        app.remove_scan(index);
        app.dragged_scan = None;
    } else if let Some((from, to)) = dropped_scan {
        app.move_scan(from, to);
        app.dragged_scan = None;
    } else if released {
        if let (Some(from), Some(to)) = (app.dragged_scan.take(), drop_target) {
            app.move_scan(from, to);
        }
    } else if app.dragged_scan.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    if remove.is_none() {
        if let Some(index) = select {
            app.select_scan(index);
        }
    }
}

fn action_button(ui: &mut egui::Ui, icon: &str, label: &str, enabled: bool) -> bool {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(""))
            .min_size(Vec2::new(ui.available_width(), 42.))
            .fill(Color32::from_rgb(57, 91, 128))
            .corner_radius(CornerRadius::same(9)),
    );
    let icon_rect = Rect::from_min_size(
        Pos2::new(response.rect.left() + 10., response.rect.center().y - 10.),
        Vec2::splat(20.),
    );
    let mut icon_ui = ui.new_child(egui::UiBuilder::new().max_rect(icon_rect));
    ScanBarcode::icon(
        &mut icon_ui,
        icon,
        if enabled {
            Color32::from_rgb(239, 242, 248)
        } else {
            Color32::from_rgb(145, 158, 177)
        },
    );
    ui.painter().text(
        Pos2::new(response.rect.left() + 40., response.rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.),
        if enabled {
            Color32::WHITE
        } else {
            Color32::from_rgb(165, 174, 189)
        },
    );
    if ui.rect_contains_pointer(response.rect) {
        ui.ctx().set_cursor_icon(if enabled {
            egui::CursorIcon::PointingHand
        } else {
            egui::CursorIcon::NotAllowed
        });
    }
    response.clicked()
}

fn action_icon(action: Option<&str>) -> &'static str {
    match action.unwrap_or_default() {
        "Connect to Wi-Fi" | "Configure Wi-Fi connection" => "wifi",
        "Add contact" | "Import contact" | "Import business contact" => "book-user",
        "Compose email" | "Send email" => "mail",
        "Compose SMS" | "Compose multimedia message" | "Send SMS" => "message-circle",
        "Prepare bank transfer"
        | "Prepare bank payment"
        | "Initiate payment"
        | "Open payment link" => "wallet-minimal",
        "Add calendar event" => "calendar",
        "Install eSIM profile" => "smartphone",
        "Pair smart-home device" | "Pair HomeKit accessory" => "home",
        "Retrieve or verify public key" => "key",
        "Import wallet data or signing request" => "wallet",
        "Look up product / inventory record" => "package",
        "Open URL" => "globe",
        "Open magnet link" => "external-link",
        "Open CorePass profile" | "Open CorePass" => "user",
        "Open CorePass chat" => "message-circle",
        "Open Tone MiniApp" => "box",
        "Open Tone token" | "Open Tone asset" => "wallet",
        "Open Tone link" | "Open Tone order" => "audio-waveform",
        "Import exchange rates" | "Update exchange rates" => "refresh",
        action if action.starts_with("Open ") => "link",
        _ => "scan-square",
    }
}

fn load_image(app: &mut ScanBarcode) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter(
            "Images",
            &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff"],
        )
        .pick_file()
    {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("image");
        let mut count = 0;
        let is_animated = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"));
        if is_animated {
            match fs::File::open(&path)
                .ok()
                .and_then(|file| GifDecoder::new(BufReader::new(file)).ok())
                .and_then(|decoder| decoder.into_frames().collect_frames().ok())
            {
                Some(frames) => count += scan_gif_frames(app, frames),
                None => app.say(format!("Could not open animated image {name}")),
            }
        } else if let Ok(image) = image::open(&path) {
            let width = image.width();
            let height = image.height();
            count += scan_image_frame(app, width, height, image.to_luma8().into_raw());
        } else {
            app.say(format!("Could not open {name}"));
        }
        if count == 0 {
            app.say(format!("No barcode found in {name}"));
        } else if app.barcode_tx.is_active() {
            app.say(format!(
                "Read {count} frame{} from {name}; animated transfer is still loading",
                if count == 1 { "" } else { "s" }
            ));
        } else {
            app.say(format!(
                "Found {count} barcode{} in {name}",
                if count == 1 { "" } else { "s" }
            ));
        }
    }
}

fn scan_gif_frames(app: &mut ScanBarcode, frames: Vec<image::Frame>) -> usize {
    frames
        .into_iter()
        .map(|frame| {
            let image = frame.into_buffer();
            scan_image_frame(
                app,
                image.width(),
                image.height(),
                image::DynamicImage::ImageRgba8(image).to_luma8().into_raw(),
            )
        })
        .sum()
}

fn scan_image_frame(app: &mut ScanBarcode, width: u32, height: u32, pixels: Vec<u8>) -> usize {
    let codes = decode_luma(pixels, width, height);
    let count = codes.len();
    for code in codes {
        app.record_decoded(code);
    }
    count
}

fn classify_scan(value: &str) -> (Option<String>, Option<String>) {
    let trimmed = value.trim();
    let first_line = trimmed.lines().next().unwrap_or_default();
    if first_line.eq_ignore_ascii_case("BEGIN:RATES") {
        return (Some("Import exchange rates".into()), Some(trimmed.into()));
    }
    if has_prefix(trimmed, "WIFI:") {
        return (Some("Connect to Wi-Fi".into()), Some(trimmed.into()));
    }
    if has_prefix(trimmed, "DPP:") {
        return (
            Some("Configure Wi-Fi connection".into()),
            Some(trimmed.into()),
        );
    }
    if has_prefix(trimmed, "BEGIN:VCARD") {
        return (Some("Add contact".into()), Some(trimmed.into()));
    }
    if has_prefix(trimmed, "MECARD:") {
        return (Some("Import contact".into()), Some(trimmed.into()));
    }
    if has_prefix(trimmed, "BIZCARD:") {
        return (Some("Import business contact".into()), Some(trimmed.into()));
    }
    if has_prefix(trimmed, "MATMSG:") || has_prefix(trimmed, "SMTP:") {
        return (Some("Compose email".into()), Some(trimmed.into()));
    }
    if has_prefix(trimmed, "SMSTO:") {
        return (Some("Compose SMS".into()), Some(trimmed.into()));
    }
    if has_prefix(trimmed, "MMSTO:") {
        return (
            Some("Compose multimedia message".into()),
            Some(trimmed.into()),
        );
    }
    if has_prefix(trimmed, "BCD") {
        return (Some("Prepare bank transfer".into()), Some(trimmed.into()));
    }
    if has_prefix(trimmed, "SPC") {
        return (Some("Prepare bank payment".into()), Some(trimmed.into()));
    }
    if has_prefix(trimmed, "BEGIN:VCALENDAR") || has_prefix(trimmed, "BEGIN:VEVENT") {
        return (Some("Add calendar event".into()), Some(trimmed.into()));
    }
    if has_prefix(trimmed, "LPA:1$") {
        return (Some("Install eSIM profile".into()), Some(trimmed.into()));
    }
    if has_prefix(trimmed, "MT:") {
        return (Some("Pair smart-home device".into()), Some(trimmed.into()));
    }
    if has_prefix(trimmed, "X-HM://") {
        return (Some("Pair HomeKit accessory".into()), Some(trimmed.into()));
    }
    if has_prefix(trimmed, "OPENPGP4FPR:") {
        return (
            Some("Retrieve or verify public key".into()),
            Some(trimmed.into()),
        );
    }
    if has_prefix(trimmed, "UR:") {
        return (
            Some("Import wallet data or signing request".into()),
            Some(trimmed.into()),
        );
    }
    if has_prefix(trimmed, "EMV:") || has_prefix(trimmed, "EMVCO:") || looks_like_emv_tlv(trimmed) {
        return (Some("Initiate payment".into()), Some(trimmed.into()));
    }
    if looks_like_gs1(trimmed) {
        return (
            Some("Look up product / inventory record".into()),
            Some(trimmed.into()),
        );
    }
    let Some((scheme, _)) = trimmed.split_once(':') else {
        return (None, None);
    };
    if !is_valid_uri_scheme(scheme) {
        return (None, None);
    }
    let scheme = scheme.to_ascii_lowercase();
    if scheme == "tone" {
        let host = trimmed
            .split_once("://")
            .and_then(|(_, value)| value.split(['/', '?', '#']).next())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let action = match host.as_str() {
            "user" => "Open CorePass profile",
            "chat" => "Open CorePass chat",
            "rates" => "Update exchange rates",
            "token" => "Open Tone token",
            "asset" => "Open Tone asset",
            "app" => "Open Tone MiniApp",
            "order" => "Open Tone order",
            _ => "Open Tone link",
        };
        return (Some(action.into()), Some(trimmed.into()));
    }
    let action = match scheme.as_str() {
        "http" | "https" => "Open URL".into(),
        "mailto" => "Send email".into(),
        "tel" => "Call phone number".into(),
        "sms" => "Send SMS".into(),
        "payto" => "Open payment link".into(),
        "magnet" => "Open magnet link".into(),
        "corepass" => "Open CorePass".into(),
        _ => format!("Open {scheme}"),
    };
    (Some(action), Some(trimmed.into()))
}

fn count_rate_entries(value: &str) -> usize {
    let lines: Vec<_> = value.lines().map(str::trim).collect();
    if !lines
        .first()
        .is_some_and(|line| line.eq_ignore_ascii_case("BEGIN:RATES"))
    {
        return 0;
    }
    let Some(end) = lines
        .iter()
        .position(|line| line.eq_ignore_ascii_case("END:RATES"))
    else {
        return 0;
    };
    if end < 1 || lines[end + 1..].iter().any(|line| !line.is_empty()) {
        return 0;
    }
    lines[1..end]
        .iter()
        .filter(|line| {
            let Some((pair, value)) = line.split_once(':') else {
                return false;
            };
            let mut pair_parts = pair.split('/');
            pair_parts.next().is_some_and(|source| {
                (3..=12).contains(&source.len())
                    && source
                        .chars()
                        .all(|character| character.is_ascii_alphabetic())
            }) && pair_parts.next().is_some_and(|quote| {
                (3..=12).contains(&quote.len())
                    && quote
                        .chars()
                        .all(|character| character.is_ascii_alphabetic())
            }) && value
                .parse::<f64>()
                .is_ok_and(|number| number.is_finite() && number > 0.)
        })
        .count()
}

fn has_prefix(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

fn looks_like_emv_tlv(value: &str) -> bool {
    value.len() >= 20
        && value.starts_with("000201")
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn looks_like_gs1(value: &str) -> bool {
    value.starts_with("]C1") || value.starts_with("(01)") || value.starts_with("01")
}

fn is_valid_uri_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic())
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

#[derive(Debug)]
struct WifiPayload {
    ssid: String,
    password: String,
}

fn parse_wifi_payload(value: &str) -> Option<WifiPayload> {
    let body = value.strip_prefix("WIFI:")?;
    let mut ssid = None;
    let mut password = String::new();
    for field in body.split(';') {
        let Some((key, field_value)) = field.split_once(':') else {
            continue;
        };
        match key.to_ascii_uppercase().as_str() {
            "S" => ssid = Some(unescape_wifi_value(field_value)),
            "P" => password = unescape_wifi_value(field_value),
            _ => {}
        }
    }
    let ssid = ssid?;
    (!ssid.is_empty()).then_some(WifiPayload { ssid, password })
}

fn unescape_wifi_value(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            result.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            result.push(character);
        }
    }
    if escaped {
        result.push('\\');
    }
    result
}

fn execute_wifi_payload(value: &str) -> bool {
    let Some(payload) = parse_wifi_payload(value) else {
        return false;
    };
    #[cfg(target_os = "linux")]
    {
        let mut command = Command::new("nmcli");
        command.args(["device", "wifi", "connect", &payload.ssid]);
        if !payload.password.is_empty() {
            command.args(["password", &payload.password]);
        }
        return command.spawn().is_ok();
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let _ = (&payload.ssid, &payload.password);
        open_external(if cfg!(target_os = "macos") {
            "x-apple.systempreferences:com.apple.wifi-settings"
        } else {
            "ms-settings:network-wifi"
        });
        true
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = payload;
        false
    }
}

fn execute_vcard(value: &str) -> bool {
    let filename = format!(
        "scanbarcode-contact-{}.vcf",
        Local::now().format("%Y%m%d%H%M%S")
    );
    let path = env::temp_dir().join(filename);
    if fs::write(&path, value).is_err() {
        return false;
    }
    open_external(&path.to_string_lossy());
    true
}

fn execute_structured_contact(value: &str) -> bool {
    let Some((prefix, body)) = (if has_prefix(value, "MECARD:") {
        Some(("MECARD:", value[prefix_len(value, "MECARD:")..].to_owned()))
    } else if has_prefix(value, "BIZCARD:") {
        Some((
            "BIZCARD:",
            value[prefix_len(value, "BIZCARD:")..].to_owned(),
        ))
    } else {
        None
    }) else {
        return false;
    };
    let fields = structured_fields(&body);
    let full_name = fields
        .get("N")
        .or_else(|| fields.get("FN"))
        .cloned()
        .unwrap_or_default();
    let mut vcard = String::from("BEGIN:VCARD\nVERSION:3.0\n");
    if !full_name.is_empty() {
        vcard.push_str(&format!("FN:{full_name}\n"));
    }
    for key in ["TEL", "TEL1", "TEL2", "TEL3"] {
        if let Some(number) = fields.get(key) {
            vcard.push_str(&format!("TEL:{number}\n"));
        }
    }
    for key in ["EMAIL", "E"] {
        if let Some(email) = fields.get(key) {
            vcard.push_str(&format!("EMAIL:{email}\n"));
        }
    }
    if prefix == "BIZCARD:" {
        if let Some(company) = fields.get("C") {
            vcard.push_str(&format!("ORG:{company}\n"));
        }
        if let Some(title) = fields.get("T") {
            vcard.push_str(&format!("TITLE:{title}\n"));
        }
    }
    vcard.push_str("END:VCARD\n");
    execute_vcard(&vcard)
}

fn prefix_len(value: &str, prefix: &str) -> usize {
    value
        .char_indices()
        .nth(prefix.chars().count())
        .map(|(index, _)| index)
        .unwrap_or(value.len())
}

fn structured_fields(value: &str) -> std::collections::HashMap<String, String> {
    value
        .split(';')
        .filter_map(|field| field.split_once(':'))
        .map(|(key, value)| (key.to_ascii_uppercase(), unescape_wifi_value(value)))
        .collect()
}

fn execute_email_payload(value: &str) -> bool {
    let (to, subject, body) = if has_prefix(value, "MATMSG:") {
        let fields = structured_fields(&value[prefix_len(value, "MATMSG:")..]);
        (
            fields.get("TO").cloned().unwrap_or_default(),
            fields.get("SUB").cloned().unwrap_or_default(),
            fields.get("BODY").cloned().unwrap_or_default(),
        )
    } else if has_prefix(value, "SMTP:") {
        let fields: Vec<_> = value[prefix_len(value, "SMTP:")..].splitn(3, ':').collect();
        (
            fields.first().copied().unwrap_or_default().into(),
            fields.get(1).copied().unwrap_or_default().into(),
            fields.get(2).copied().unwrap_or_default().into(),
        )
    } else {
        return false;
    };
    if to.is_empty() {
        return false;
    }
    let mut uri = format!("mailto:{}", url_encode(&to));
    let mut query = Vec::new();
    if !subject.is_empty() {
        query.push(format!("subject={}", url_encode(&subject)));
    }
    if !body.is_empty() {
        query.push(format!("body={}", url_encode(&body)));
    }
    if !query.is_empty() {
        uri.push('?');
        uri.push_str(&query.join("&"));
    }
    open_external(&uri);
    true
}

fn execute_sms_payload(value: &str) -> bool {
    let prefix = if has_prefix(value, "SMSTO:") {
        "SMSTO:"
    } else if has_prefix(value, "MMSTO:") {
        "MMSTO:"
    } else {
        return false;
    };
    let fields: Vec<_> = value[prefix_len(value, prefix)..].splitn(2, ':').collect();
    let number = fields.first().copied().unwrap_or_default();
    if number.is_empty() {
        return false;
    }
    let message = fields.get(1).copied().unwrap_or_default();
    let uri = if message.is_empty() {
        format!("sms:{}", url_encode(number))
    } else {
        format!("sms:{}?body={}", url_encode(number), url_encode(message))
    };
    open_external(&uri);
    true
}

fn execute_calendar_payload(value: &str) -> bool {
    let calendar = if has_prefix(value, "BEGIN:VCALENDAR") {
        value.to_owned()
    } else {
        format!("BEGIN:VCALENDAR\nVERSION:2.0\n{value}\nEND:VCALENDAR\n")
    };
    let path = env::temp_dir().join(format!(
        "scanbarcode-calendar-{}.ics",
        Local::now().format("%Y%m%d%H%M%S")
    ));
    if fs::write(&path, calendar).is_err() {
        return false;
    }
    open_external(&path.to_string_lossy());
    true
}

fn url_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn open_external(value: &str) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(value).spawn();
    #[cfg(target_os = "linux")]
    let _ = Command::new("xdg-open").arg(value).spawn();
    #[cfg(target_os = "windows")]
    let _ = Command::new("cmd").args(["/C", "start", "", value]).spawn();
}

fn play_scan_beep() {
    thread::spawn(|| {
        let Ok(mut speakers) = DeviceSinkBuilder::open_default_sink() else {
            return;
        };
        speakers.log_on_drop(false);
        speakers.mixer().add(
            SineWave::new(880.0)
                .take_duration(Duration::from_millis(90))
                .amplify(0.16),
        );
        thread::sleep(Duration::from_millis(120));
    });
}

fn decode_luma(luma: Vec<u8>, width: u32, height: u32) -> Vec<DetectedCode> {
    rxing::helpers::detect_multiple_in_luma(luma, width, height)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|result| {
            let text = result.getText().trim().to_owned();
            (!text.is_empty() || is_barcode_tx_frame(result.getRawBytes())).then(|| DetectedCode {
                text,
                format: result.getBarcodeFormat().to_string(),
                raw_bytes: result.getRawBytes().to_vec(),
            })
        })
        .collect()
}

fn is_barcode_tx_frame(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[..2] == *b"BX"
}

fn rgb_to_luma(pixels: &[u8]) -> Vec<u8> {
    pixels
        .chunks_exact(3)
        .map(|pixel| {
            ((u32::from(pixel[0]) * 77 + u32::from(pixel[1]) * 150 + u32::from(pixel[2]) * 29) >> 8)
                as u8
        })
        .collect()
}

fn menu_item(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    shortcut: &str,
    enabled: bool,
    _checked: bool,
) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.), Sense::click());
    if response.hovered() && enabled {
        ui.painter()
            .rect_filled(rect, 6., Color32::from_rgb(52, 55, 65));
    }
    let text_color = if enabled {
        Color32::from_rgb(232, 233, 238)
    } else {
        Color32::from_rgb(110, 112, 121)
    };
    let mut icon_ui = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
        Pos2::new(rect.left() + 8., rect.center().y - 10.),
        Vec2::splat(20.),
    )));
    ScanBarcode::icon(&mut icon_ui, icon, text_color);
    ui.painter().text(
        Pos2::new(rect.left() + 34., rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.),
        text_color,
    );
    ui.painter().text(
        Pos2::new(rect.right() - 10., rect.center().y),
        egui::Align2::RIGHT_CENTER,
        shortcut,
        FontId::proportional(12.),
        Color32::from_rgb(150, 152, 161),
    );
    if ui.rect_contains_pointer(response.rect) {
        ui.ctx().set_cursor_icon(if enabled {
            egui::CursorIcon::PointingHand
        } else {
            egui::CursorIcon::NotAllowed
        });
    }
    response.clicked() && enabled
}

fn is_newer_version(candidate: &str, current: &str) -> bool {
    let parse = |value: &str| -> Vec<u32> {
        value
            .trim_start_matches('v')
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    let candidate = parse(candidate);
    let current = parse(current);
    (0..candidate.len().max(current.len()))
        .map(|index| {
            (
                candidate.get(index).copied().unwrap_or(0),
                current.get(index).copied().unwrap_or(0),
            )
        })
        .find(|(next, now)| next != now)
        .map(|(next, now)| next > now)
        .unwrap_or(false)
}

fn format_download_progress(downloaded: u64, total: Option<u64>) -> String {
    let megabytes = |bytes: u64| bytes as f64 / 1_048_576.0;
    match total {
        Some(total) => format!("{:.1} of {:.1} MB", megabytes(downloaded), megabytes(total)),
        None => format!("{:.1} MB downloaded", megabytes(downloaded)),
    }
}

fn camera_loop(
    initial_index: usize,
    commands: Receiver<CameraCommand>,
    frames: SyncSender<CameraFrame>,
) {
    let requested = RequestedFormat::new::<RgbFormat>(RequestedFormatType::HighestResolution(
        Resolution::new(640, 480),
    ));
    let mut current_index = initial_index;
    let mut running = true;
    let mut camera: Option<Camera> = None;
    let mut frame_number = 0u32;

    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                CameraCommand::SetCamera(index) => {
                    if let Some(mut active) = camera.take() {
                        let _ = active.stop_stream();
                    }
                    current_index = index;
                    running = true;
                }
                CameraCommand::SetRunning(value) => {
                    running = value;
                    if !running {
                        if let Some(mut active) = camera.take() {
                            let _ = active.stop_stream();
                        }
                    }
                }
            }
        }
        if !running {
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        if camera.is_none() {
            match Camera::new(CameraIndex::Index(current_index as u32), requested.clone()) {
                Ok(mut active) => {
                    if active.open_stream().is_ok() {
                        camera = Some(active);
                    } else {
                        thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                }
                Err(_) => {
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            }
        }
        if let Some(active) = &mut camera {
            if let Ok(frame) = active.frame() {
                if let Ok(image) = frame.decode_image::<RgbFormat>() {
                    let width = image.width() as usize;
                    let height = image.height() as usize;
                    let pixels = image.into_raw();
                    frame_number = frame_number.wrapping_add(1);
                    let codes = if frame_number % 5 == 0 {
                        decode_luma(rgb_to_luma(&pixels), width as u32, height as u32)
                    } else {
                        Vec::new()
                    };
                    match frames.try_send(CameraFrame {
                        width,
                        height,
                        pixels,
                        codes,
                    }) {
                        Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                        Err(mpsc::TrySendError::Disconnected(_)) => return,
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(30));
    }
}

fn detect_camera_names() -> Vec<String> {
    if let Ok(value) = env::var("SCANBARCODE_CAMERA_COUNT") {
        if let Ok(count) = value.parse::<usize>() {
            return (0..count.max(1))
                .map(|index| format!("Camera {}", index + 1))
                .collect();
        }
    }
    #[cfg(target_os = "macos")]
    {
        nokhwa::nokhwa_initialize(|_| {});
    }
    if let Some(backend) = nokhwa::native_api_backend() {
        if let Ok(cameras) = nokhwa::query(backend) {
            let names: Vec<String> = cameras
                .into_iter()
                .map(|camera| camera.human_name())
                .collect();
            if !names.is_empty() {
                return names;
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("system_profiler")
            .args(["SPCameraDataType"])
            .output()
            .ok();
        if let Some(output) = output {
            let names: Vec<String> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let name = line.trim();
                    (line.starts_with("      ") && name.ends_with(':'))
                        .then(|| name.trim_end_matches(':').to_string())
                })
                .collect();
            if !names.is_empty() {
                return names;
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        let mut names: Vec<String> = fs::read_dir("/dev")
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.starts_with("video").then(|| format!("/dev/{name}"))
            })
            .collect();
        names.sort();
        if !names.is_empty() {
            return names;
        }
    }
    vec!["Camera 1".into()]
}
#[cfg(target_os = "macos")]
fn install_native_menu() {
    use muda::accelerator::{Accelerator, Code, Modifiers};
    use muda::{Menu, MenuItem, PredefinedMenuItem, Submenu};
    let cmd = Modifiers::META;
    let cmd_shift = Modifiers::META | Modifiers::SHIFT;
    let cmd_o = Some(Accelerator::new(cmd, Code::KeyO));
    let cmd_shift_c = Some(Accelerator::new(cmd_shift, Code::KeyC));
    let cmd_s = Some(Accelerator::new(cmd, Code::KeyS));
    let cmd_k = Some(Accelerator::new(cmd, Code::KeyK));
    let cmd_p = Some(Accelerator::new(cmd, Code::KeyP));
    let cmd_m = Some(Accelerator::new(cmd, Code::KeyM));
    let cmd_r = Some(Accelerator::new(cmd, Code::KeyR));
    let space = Some(Accelerator::new(Modifiers::empty(), Code::Space));
    let cmd_l = Some(Accelerator::new(cmd, Code::KeyL));
    let cmd_tab = Some(Accelerator::new(cmd, Code::Tab));
    let cmd_enter = Some(Accelerator::new(cmd, Code::Enter));
    let cmd_q = Some(Accelerator::new(cmd, Code::KeyQ));
    let shift_up = Some(Accelerator::new(Modifiers::SHIFT, Code::ArrowUp));
    let shift_down = Some(Accelerator::new(Modifiers::SHIFT, Code::ArrowDown));
    let app_menu = Submenu::with_items(
        "ScanBarcode",
        true,
        &[
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id("scanbarcode.quit", "Quit ScanBarcode", true, cmd_q),
        ],
    )
    .expect("create native ScanBarcode menu");
    let file_menu = Submenu::with_items(
        "File",
        true,
        &[
            &MenuItem::with_id("scanbarcode.load", "Load image…", true, cmd_o),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id("scanbarcode.save", "Save results…", true, cmd_s),
            &MenuItem::with_id("scanbarcode.clear", "Clear results", true, cmd_k),
        ],
    )
    .expect("create native File menu");
    let edit_menu = Submenu::with_items(
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id("scanbarcode.copy", "Copy results", true, cmd_shift_c),
            &MenuItem::with_id(
                "scanbarcode.action",
                "Open detected action",
                true,
                cmd_enter,
            ),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id("scanbarcode.move_up", "Move selected up", true, shift_up),
            &MenuItem::with_id(
                "scanbarcode.move_down",
                "Move selected down",
                true,
                shift_down,
            ),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::select_all(None),
        ],
    )
    .expect("create native Edit menu");
    let view_menu = Submenu::with_items(
        "View",
        true,
        &[
            &MenuItem::with_id("scanbarcode.camera", "Camera on / off", true, space),
            &MenuItem::with_id("scanbarcode.preview", "Show camera preview", true, cmd_p),
            &MenuItem::with_id("scanbarcode.light", "Toggle light", true, cmd_l),
            &MenuItem::with_id("scanbarcode.mute", "Mute scan sounds", true, cmd_m),
            &MenuItem::with_id("scanbarcode.mirror", "Mirror camera preview", true, cmd_r),
            &MenuItem::with_id("scanbarcode.switch_camera", "Switch camera", true, cmd_tab),
        ],
    )
    .expect("create native View menu");
    let window_menu = Submenu::with_items(
        "Window",
        true,
        &[
            &PredefinedMenuItem::zoom(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::close_window(None),
            &PredefinedMenuItem::bring_all_to_front(None),
        ],
    )
    .expect("create native Window menu");
    let help_menu = Submenu::with_items(
        "Help",
        true,
        &[&MenuItem::with_id(
            "scanbarcode.about",
            "About ScanBarcode",
            true,
            None,
        )],
    )
    .expect("create native Help menu");
    let menu = Menu::new();
    for submenu in [
        &app_menu as &dyn muda::IsMenuItem,
        &file_menu,
        &edit_menu,
        &view_menu,
        &window_menu,
        &help_menu,
    ] {
        menu.append(submenu)
            .expect("append native ScanBarcode menu");
    }
    menu.init_for_nsapp();
    Box::leak(Box::new(menu));
}

fn main() -> eframe::Result<()> {
    let _ = SUPPORTED_CODE_TYPES;
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/scanbarcode-icon.png"))
        .ok()
        .map(std::sync::Arc::new);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("ScanBarcode")
        .with_app_id("ScanBarcode")
        .with_inner_size([1100., 760.])
        .with_min_inner_size([820., 560.]);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "ScanBarcode",
        options,
        Box::new(|cc| Ok(Box::new(ScanBarcode::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        classify_scan, crc32, decode_luma, move_shortcut_direction, rgb_to_luma, scan_gif_frames,
        scan_image_frame, scan_rows, BarcodeTxDecoder, ScanBarcode,
    };
    use image::{
        codecs::gif::{GifDecoder, GifEncoder},
        AnimationDecoder, Frame, Rgba, RgbaImage,
    };
    use rxing::{qrcode::QRCodeWriter, BarcodeFormat, Writer};
    use sha2::{Digest, Sha256};
    use std::io::Cursor;

    #[test]
    fn dragging_a_row_moves_it_to_the_drop_row() {
        use egui::{pos2, vec2, Context, Event, Modifiers, PointerButton, RawInput, Rect};

        let ctx = Context::default();
        let mut app = test_app();
        app.record_scan("first", None, None);
        app.record_scan("second", None, None);
        let show = |app: &mut ScanBarcode, events| {
            let output = ctx.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0., 0.), vec2(400., 300.))),
                    events,
                    ..Default::default()
                },
                |ui| scan_rows(app, ui, 200.),
            );
            let shapes = output.shapes.clone();
            output.drop_without_applying_deltas();
            shapes
        };
        show(&mut app, vec![]);
        let source = pos2(13., 57.);
        let destination = pos2(100., 19.);
        show(
            &mut app,
            vec![
                Event::PointerMoved(source),
                Event::PointerButton {
                    pos: source,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
            ],
        );
        assert_eq!(app.dragged_scan, Some(1));
        let moving = show(&mut app, vec![Event::PointerMoved(destination)]);
        assert!(moving.iter().any(|shape| matches!(
            &shape.shape,
            egui::Shape::Rect(rect) if rect.fill == egui::Color32::from_rgb(47, 70, 88)
        )));
        show(
            &mut app,
            vec![Event::PointerButton {
                pos: destination,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert_eq!(app.scanned_items, ["second", "first"]);
        assert_eq!(app.selected_scan, Some(0));
        let other_row = pos2(100., 57.);
        show(
            &mut app,
            vec![Event::PointerButton {
                pos: other_row,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            }],
        );
        show(
            &mut app,
            vec![Event::PointerButton {
                pos: other_row,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }],
        );
        assert_eq!(app.selected_scan, Some(1));
    }

    #[test]
    fn selected_row_controls_the_detected_action() {
        let mut app = test_app();
        app.record_scan(
            "https://example.com",
            Some("Open URL".into()),
            Some("https://example.com".into()),
        );
        app.record_scan("plain text", None, None);
        assert_eq!(app.selected_scan, Some(1));
        assert!(app.last_action.is_none());
        app.select_scan(0);
        assert_eq!(app.last_action.as_deref(), Some("Open URL"));
        assert_eq!(app.last_deeplink.as_deref(), Some("https://example.com"));
        app.move_scan(0, 1);
        assert_eq!(app.selected_scan, Some(1));
        assert_eq!(app.last_action.as_deref(), Some("Open URL"));
        app.remove_scan(0);
        assert_eq!(app.selected_scan, Some(0));
        assert_eq!(app.last_action.as_deref(), Some("Open URL"));
        app.remove_scan(0);
        assert_eq!(app.selected_scan, None);
        assert!(app.last_deeplink.is_none());
    }

    #[test]
    fn shift_arrows_move_the_selected_row_one_place() {
        use egui::{Context, Event, Key, Modifiers, RawInput};

        let ctx = Context::default();
        let mut app = test_app();
        app.record_scan("first", None, None);
        app.record_scan("https://example.com", None, None);
        app.record_scan("third", None, None);
        app.select_scan(1);
        let press = |app: &mut ScanBarcode, key, modifiers| {
            let output = ctx.run_ui(
                RawInput {
                    events: vec![
                        Event::ModifiersChanged(modifiers),
                        Event::Key {
                            key,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers,
                        },
                    ],
                    ..Default::default()
                },
                |_| {
                    if let Some(direction) = ctx.input(move_shortcut_direction) {
                        app.move_selected_scan(direction);
                    }
                },
            );
            output.drop_without_applying_deltas();
        };
        press(&mut app, Key::ArrowUp, Modifiers::SHIFT);
        assert_eq!(app.scanned_items, ["https://example.com", "first", "third"]);
        assert_eq!(app.selected_scan, Some(0));
        assert_eq!(app.last_action.as_deref(), Some("Open URL"));
        press(&mut app, Key::ArrowUp, Modifiers::SHIFT);
        assert_eq!(app.selected_scan, Some(0));
        press(&mut app, Key::ArrowDown, Modifiers::SHIFT);
        assert_eq!(app.scanned_items, ["first", "https://example.com", "third"]);
        assert_eq!(app.selected_scan, Some(1));
        press(&mut app, Key::ArrowDown, Modifiers::SHIFT);
        assert_eq!(app.scanned_items, ["first", "third", "https://example.com"]);
        assert_eq!(app.selected_scan, Some(2));
        press(&mut app, Key::ArrowDown, Modifiers::SHIFT);
        assert_eq!(app.selected_scan, Some(2));
        assert_eq!(app.scanned_text, "first\nthird\nhttps://example.com");
    }

    fn test_app() -> ScanBarcode {
        ScanBarcode {
            camera_on: true,
            torch_on: false,
            camera_index: 0,
            camera_count: 1,
            camera_names: vec!["Camera 1".into()],
            scanned_text: String::new(),
            scanned_items: Vec::new(),
            selected_scan: None,
            dragged_scan: None,
            last_action: None,
            last_deeplink: None,
            last_camera_value: None,
            menu_open: false,
            mirror_camera: true,
            show_preview: true,
            mute_sound: false,
            toast: None,
            camera_command: None,
            camera_frames: None,
            camera_texture: None,
            camera_error: None,
            confirm_clear: false,
            confirm_close: false,
            close_confirmed: false,
            about_open: false,
            auto_updates: true,
            update_rx: None,
            update_status: format!("Version {}", super::APP_VERSION),
            update_prompt: false,
            update_phase: super::UpdatePhase::Idle,
            update_version: None,
            update_progress: None,
            update_downloaded: 0,
            update_total: None,
            update_archive: None,
            update_partial: None,
            update_pause: None,
            update_asset_url: None,
            update_digest: None,
            barcode_tx: BarcodeTxDecoder::default(),
        }
    }

    #[test]
    fn scans_append_on_new_lines_and_latest_action_wins() {
        let mut app = test_app();
        app.record_scan("first", None, None);
        app.record_scan(
            "second",
            Some("Open payto".into()),
            Some("payto:iban".into()),
        );
        assert_eq!(app.scanned_text, "first\nsecond");
        assert_eq!(app.scanned_items, ["first", "second"]);
        assert_eq!(app.selected_scan, Some(1));
        assert_eq!(app.last_action.as_deref(), Some("Open payto"));
        assert_eq!(app.last_deeplink.as_deref(), Some("payto:iban"));
        app.record_scan("line one\nline two", None, None);
        assert_eq!(app.scanned_items.len(), 3);
        assert_eq!(app.selected_scan, Some(2));
        app.move_scan(2, 0);
        assert_eq!(app.scanned_items[0], "line one\nline two");
        assert_eq!(app.selected_scan, Some(0));
        assert_eq!(app.scanned_text, "line one\nline two\nfirst\nsecond");
        app.remove_scan(2);
        assert_eq!(app.scanned_items, ["line one\nline two", "first"]);
        assert_eq!(app.selected_scan, Some(0));
        assert_eq!(app.scanned_text, "line one\nline two\nfirst");
        assert!(app.last_deeplink.is_none());
    }

    #[test]
    fn decoder_supports_luma_input_and_deeplink_classification() {
        assert_eq!(rgb_to_luma(&[255, 255, 255]), vec![255]);
        assert!(decode_luma(vec![255], 1, 1).is_empty());
        assert_eq!(
            classify_scan("https://example.com").0.as_deref(),
            Some("Open URL")
        );
        assert_eq!(
            classify_scan("WIFI:T:WPA;S:Office\\;Guest;P:secret;;")
                .0
                .as_deref(),
            Some("Connect to Wi-Fi")
        );
        assert_eq!(
            classify_scan("BEGIN:VCARD\nFN:Alex\nEND:VCARD")
                .0
                .as_deref(),
            Some("Add contact")
        );
        assert_eq!(
            classify_scan("payto://iban/DE123").0.as_deref(),
            Some("Open payment link")
        );
        assert_eq!(
            classify_scan("my-custom-app://open/item/42").0.as_deref(),
            Some("Open my-custom-app")
        );
        assert_eq!(
            classify_scan("BEGIN:RATES\nUSD/EUR:0.92\nEND:RATES")
                .0
                .as_deref(),
            Some("Import exchange rates")
        );
        assert_eq!(classify_scan("RATES\nUSD/EUR:0.92"), (None, None));
        assert_eq!(
            classify_scan("tone://rates/update").0.as_deref(),
            Some("Update exchange rates")
        );
        assert_eq!(
            classify_scan("magnet:?xt=urn:btih:0123456789abcdef")
                .0
                .as_deref(),
            Some("Open magnet link")
        );
        assert_eq!(
            classify_scan("corepass:CB123").0.as_deref(),
            Some("Open CorePass")
        );
        for (payload, action) in [
            ("DPP:VERSION=1;", "Configure Wi-Fi connection"),
            ("MECARD:N:Alex;TEL:+421555123;", "Import contact"),
            (
                "BIZCARD:N:Alex;C:Example;E:a@example.com;",
                "Import business contact",
            ),
            (
                "MATMSG:TO:a@example.com;SUB:Hello;BODY:World;;",
                "Compose email",
            ),
            ("SMTP:a@example.com:Hello:World;", "Compose email"),
            ("SMSTO:+421555123:Hello", "Compose SMS"),
            ("MMSTO:+421555123:Photo", "Compose multimedia message"),
            ("BCD\n001\n1\nSCT", "Prepare bank transfer"),
            ("SPC\n0200", "Prepare bank payment"),
            ("BEGIN:VCALENDAR\nBEGIN:VEVENT", "Add calendar event"),
            ("LPA:1$smdp.example$code", "Install eSIM profile"),
            ("MT:CH:123", "Pair smart-home device"),
            ("X-HM://ABC123", "Pair HomeKit accessory"),
            ("OPENPGP4FPR:ABC123", "Retrieve or verify public key"),
            (
                "UR:crypto-request/abc",
                "Import wallet data or signing request",
            ),
            ("00020101021229370016A000000727012700", "Initiate payment"),
            ("(01)09506000134352", "Look up product / inventory record"),
        ] {
            assert_eq!(
                classify_scan(payload).0.as_deref(),
                Some(action),
                "{payload}"
            );
        }
        assert_eq!(classify_scan("plain text"), (None, None));
    }

    #[test]
    fn barcode_tx_simulation_reconstructs_a_two_transaction_batch() {
        let transactions = [
            (b"XCB", b"first".as_slice()),
            (b"XAB", b"second".as_slice()),
        ];
        let mut batch = b"BTXB".to_vec();
        batch.extend([1, 0, transactions.len() as u8]);
        for (blockchain, payload) in transactions {
            batch.extend(blockchain);
            batch.push(0);
            batch.extend((payload.len() as u32).to_be_bytes());
            batch.extend(payload);
        }
        let digest = Sha256::digest(&batch);
        let mut protected = batch;
        protected.extend(digest);
        assert!(protected.len() <= 773);

        let mut frame = b"BX".to_vec();
        frame.extend([1, 0]);
        frame.extend(&digest[..6]);
        frame.extend((protected.len() as u32).to_be_bytes());
        frame.extend((773u16).to_be_bytes());
        frame.extend((1u16).to_be_bytes());
        frame.extend(0u32.to_be_bytes());
        frame.push(1);
        frame.extend(&protected);
        frame.resize(23 + 773, 0);
        let checksum = crc32(&frame);
        frame.extend(checksum.to_be_bytes());

        let mut decoder = BarcodeTxDecoder::default();
        assert!(decoder.add_frame(&frame).unwrap());
        let result = decoder.result.expect("simulated batch should complete");
        assert_eq!(result.transactions, 2);
        assert_eq!(result.supported_xcb, 1);
        assert_eq!(result.items.len(), 2);
        assert!(result.items[0].contains("XCB"));
        assert!(result.items[1].contains("XAB"));
        let mut app = test_app();
        app.record_barcode_tx_frame(&frame);
        assert_eq!(app.scanned_items, result.items);
        assert_eq!(app.scanned_items.len(), 2);
        assert_eq!(app.scanned_text.lines().count(), 2);
        assert_eq!(app.selected_scan, Some(1));
        assert!(app.last_action.is_none());
    }

    #[test]
    fn barcode_tx_decodes_tone_app_transaction_batch() {
        // Generated by Tone's pinned flutter_barcodetx 0.2.0 encoder.
        let frames: Vec<Vec<u8>> = include_str!("../tests/fixtures/tone_barcodetx_frames.txt")
            .lines()
            .map(|hex| {
                hex.as_bytes()
                    .chunks_exact(2)
                    .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                    .collect()
            })
            .collect();
        assert_eq!(frames.len(), 5);
        let mut app = test_app();
        // A repair symbol and source symbols can arrive in any order.
        for index in [3, 1, 3, 0] {
            app.record_barcode_tx_frame(&frames[index]);
        }
        assert_eq!(app.barcode_tx.received_symbols(), 2);
        assert_eq!(app.scanned_items.len(), 3);
        assert_eq!(app.scanned_items[0], "BarcodeTx XCB transaction: a1b2c3d4");
        assert_eq!(app.scanned_items[1], "BarcodeTx XAB transaction: 01020304");
        assert_eq!(
            app.scanned_items[2],
            format!("BarcodeTx ETH transaction: {}", "ab".repeat(800))
        );
        assert_eq!(app.scanned_text.lines().count(), 3);
        assert_eq!(app.selected_scan, Some(2));
    }

    #[test]
    fn barcode_tx_scans_tone_app_binary_qr_images() {
        let mut app = test_app();
        for png in [
            include_bytes!("../tests/fixtures/tone_barcodetx_frame_3.png").as_slice(),
            include_bytes!("../tests/fixtures/tone_barcodetx_frame_1.png").as_slice(),
        ] {
            let image = image::load_from_memory(png).unwrap().to_luma8();
            let (width, height) = image.dimensions();
            assert_eq!(
                scan_image_frame(&mut app, width, height, image.into_raw()),
                1
            );
        }
        assert_eq!(app.scanned_items.len(), 3);
        assert_eq!(app.selected_scan, Some(2));
    }

    #[test]
    fn animated_gif_scans_each_qr_into_its_own_row() {
        let frames = ["animated first", "animated second"].map(|value| {
            let matrix = QRCodeWriter::default()
                .encode(value, &BarcodeFormat::QR_CODE, 256, 256)
                .unwrap();
            Frame::new(RgbaImage::from_fn(256, 256, |x, y| {
                let shade = if matrix.get(x, y) { 0 } else { 255 };
                Rgba([shade, shade, shade, 255])
            }))
        });
        let mut gif = Vec::new();
        GifEncoder::new(&mut gif).encode_frames(frames).unwrap();
        let decoded = GifDecoder::new(Cursor::new(gif))
            .unwrap()
            .into_frames()
            .collect_frames()
            .unwrap();
        let mut app = test_app();
        assert_eq!(scan_gif_frames(&mut app, decoded), 2);
        assert_eq!(app.scanned_items, ["animated first", "animated second"]);
        assert_eq!(app.scanned_text, "animated first\nanimated second");
    }
}
