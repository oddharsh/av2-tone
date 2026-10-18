//! The first tile group's header in a closed-loop key OBU: the
//! first-tile-group bit, uncompressed_header() for a single-picture key
//! frame, then the tile indices. AVM: av2_read_tilegroup_header and
//! read_uncompressed_header in av2/decoder/decodeframe.c, and the setup_*
//! functions they call, in that order.

use crate::bits::BitReader;
use crate::error::{Error, Result};
use crate::obu::OBU_CLOSED_LOOP_KEY;
use crate::seq::{
    self, CDEF_ON_SKIP_TXFM_ADAPTIVE, MAX_SEGMENTS, MAX_SEGMENTS_8, SEG_LVL_ALT_Q, SEG_LVL_MAX, SEQ_LEVEL_MAX,
    SequenceHeader,
};

const MAX_TILE_COLS: u32 = 64;
const MAX_TILE_ROWS: u32 = 64;
const MAX_TILE_WIDTH: u32 = 4096;
const MAX_TILE_AREA: u32 = 4096 * 2304;
const MAX_MFH_NUM: u32 = 16;
const MAX_SEQ_NUM: u32 = 16;
const QM_LEVEL_BITS: u32 = 4;
const CDEF_STRENGTH_BITS: u32 = 6;
const GDF_TEST_BLK_SIZE: u32 = 128;
const RESTORATION_UNITSIZE_MAX: u32 = 512;
/// CCSO_BLK_SIZE, which AVM sets to MAX_SB_SIZE_LOG2.
const CCSO_BLK_SIZE_LOG2: u32 = 8;
const CCSO_BAND_NUM_LOG2: u32 = 6;
const CCSO_OFFSET: [i32; 8] = [0, 1, -1, 3, -3, 7, -7, -10];
const CCSO_QUANT_SZ: [[u16; 4]; 4] = [[16, 8, 32, 0], [56, 40, 64, 128], [48, 24, 96, 192], [80, 112, 160, 256]];
const CCSO_EDGE_INTERVAL: [usize; 2] = [3, 2];
/// MAXQ_8_BITS, MAXQ_10_BITS, MAXQ.
const fn max_qindex(bit_depth: u32) -> i32 {
    match bit_depth {
        8 => 255,
        10 => 255 + 2 * 24,
        _ => 255 + 4 * 24,
    }
}

/// [tier][level], each times MAX_TILE_WIDTH / 4 (tile_common.h).
const TILE_WIDTH_SCALE: [[u32; 31]; 2] = [
    [4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 8, 8, 8, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 8, 8, 8, 8, 16, 16, 16, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0],
];
/// [tier][level], each times MAX_TILE_AREA / 4.
const TILE_AREA_SCALE: [[u32; 31]; 2] = [
    [4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 8, 8, 8, 8, 16, 16, 16, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 16, 16, 16, 16, 32, 32, 32, 32, 0, 0, 0, 0, 0, 0, 0, 0, 0],
];

pub const TX_MODE_ONLY_4X4: u32 = 0;
pub const TX_MODE_LARGEST: u32 = 1;
pub const TX_MODE_SELECT: u32 = 2;

/// Smallest k with blk << k >= target (tile_common.h).
fn tile_log2(blk: u32, target: u32) -> u32 {
    let mut k = 0;
    while (u64::from(blk) << k) < u64::from(target) {
        k += 1;
    }
    k
}

fn align_pow2(v: u32, n: u32) -> u32 {
    (v + ((1 << n) - 1)) & !((1 << n) - 1)
}

fn at<T: Copy>(table: &[T], i: usize, what: &'static str) -> Result<T> {
    table.get(i).copied().ok_or(Error::Invalid(what))
}

/// The tile grid: av2_get_tile_limits, read_tile_info_max_tile and
/// av2_calculate_tile_cols/rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tiles {
    pub uniform: bool,
    pub cols: u32,
    pub rows: u32,
    pub log2_cols: u32,
    pub log2_rows: u32,
    /// cols + 1 entries, in superblocks.
    pub col_start_sb: Vec<u32>,
    /// rows + 1 entries, in superblocks.
    pub row_start_sb: Vec<u32>,
    pub context_update_tile_id: u32,
    pub tile_size_bytes: u32,
    pub sb_cols: u32,
    pub sb_rows: u32,
    min_log2_cols: u32,
    max_log2_cols: u32,
    max_log2_rows: u32,
    min_log2: u32,
    min_log2_rows: u32,
    max_width_sb: u32,
    max_height_sb: u32,
    mib_size_log2: u32,
    scale_sb: u32,
    mi_cols: u32,
    mi_rows: u32,
    min_inner_width: i64,
}

impl Tiles {
    fn limits(mi_rows: u32, mi_cols: u32, mib_size_log2: u32, seq: &SequenceHeader) -> Result<Tiles> {
        let mut t = Tiles { mib_size_log2, mi_rows, mi_cols, ..Default::default() };
        t.scale_sb = seq.mib_size_log2() - mib_size_log2;
        t.sb_cols = align_pow2(mi_cols, mib_size_log2) >> mib_size_log2;
        t.sb_rows = align_pow2(mi_rows, mib_size_log2) >> mib_size_log2;
        let sb_size_log2 = mib_size_log2 + 2;
        let (max_width_sb, max_area_sb) = if seq.level == SEQ_LEVEL_MAX {
            (t.sb_cols, t.sb_cols * t.sb_rows)
        } else {
            let tier = usize::from(seq.tier > 0);
            let lvl = seq.level as usize;
            let w = TILE_WIDTH_SCALE.get(tier).and_then(|r| r.get(lvl)).copied().ok_or(Error::Invalid("level"))?;
            let a = TILE_AREA_SCALE.get(tier).and_then(|r| r.get(lvl)).copied().ok_or(Error::Invalid("level"))?;
            (
                (w * MAX_TILE_WIDTH) >> (sb_size_log2 + 2),
                ((u64::from(a) * u64::from(MAX_TILE_AREA)) >> (2 * sb_size_log2 + 2)) as u32,
            )
        };
        t.max_width_sb = max_width_sb;
        t.min_log2_cols = tile_log2(max_width_sb, t.sb_cols);
        t.max_log2_cols = tile_log2(1, t.sb_cols.min(MAX_TILE_COLS));
        t.max_log2_rows = tile_log2(1, t.sb_rows.min(MAX_TILE_ROWS));
        t.min_log2 = tile_log2(max_area_sb, t.sb_cols * t.sb_rows).max(t.min_log2_cols);
        Ok(t)
    }

    /// read_tile_info_max_tile.
    fn read(&mut self, r: &mut BitReader) -> Result<()> {
        self.uniform = r.bit()?;
        if self.uniform {
            self.log2_cols = self.min_log2_cols;
            while self.log2_cols < self.max_log2_cols && r.bit()? {
                self.log2_cols += 1;
            }
        } else {
            self.col_start_sb = Vec::new();
            let (mut start, mut left) = (0, self.sb_cols);
            while left > 0 && (self.col_start_sb.len() as u32) < MAX_TILE_COLS {
                let size = 1 + r.quniform(left.min(self.max_width_sb))?;
                self.col_start_sb.push(start);
                start += size;
                left = left.saturating_sub(size);
            }
            self.cols = self.col_start_sb.len() as u32;
            self.col_start_sb.push(start + left);
        }
        self.calculate_cols();
        if self.uniform {
            self.log2_rows = self.min_log2_rows;
            while self.log2_rows < self.max_log2_rows && r.bit()? {
                self.log2_rows += 1;
            }
        } else {
            self.row_start_sb = Vec::new();
            let (mut start, mut left) = (0, self.sb_rows);
            while left > 0 && (self.row_start_sb.len() as u32) < MAX_TILE_ROWS {
                let size = 1 + r.quniform(left.min(self.max_height_sb))?;
                self.row_start_sb.push(start);
                start += size;
                left = left.saturating_sub(size);
            }
            self.rows = self.row_start_sb.len() as u32;
            self.row_start_sb.push(start + left);
        }
        self.calculate_rows();
        Ok(())
    }

    fn calculate_cols(&mut self) {
        let sb_cols = align_pow2(self.mi_cols, self.mib_size_log2) >> self.mib_size_log2;
        let sb_rows = align_pow2(self.mi_rows, self.mib_size_log2) >> self.mib_size_log2;
        self.min_inner_width = -1;
        if self.uniform {
            let seq_mib = self.mib_size_log2 + self.scale_sb;
            let seq_sb_cols = align_pow2(self.mi_cols, seq_mib) >> seq_mib;
            let full = self.mi_cols >> seq_mib;
            let base = full >> self.log2_cols;
            let mut extra = full - (base << self.log2_cols);
            if base == 0 {
                extra += seq_sb_cols - full;
            }
            let size_sb = if full > 0 { (base + u32::from(extra > 0)) << self.scale_sb } else { 1 };
            self.col_start_sb.clear();
            let mut start = 0;
            while start < seq_sb_cols && (self.col_start_sb.len() as u64) < (1u64 << self.log2_cols) {
                self.col_start_sb.push(start << self.scale_sb);
                start += base + u32::from(extra > 0);
                extra = extra.saturating_sub(1);
            }
            self.cols = self.col_start_sb.len() as u32;
            self.log2_cols = tile_log2(1, self.cols);
            self.col_start_sb.push(sb_cols);
            self.min_log2_rows = self.min_log2.saturating_sub(self.log2_cols);
            self.max_height_sb = sb_rows >> self.min_log2_rows;
            let width = (size_sb << self.mib_size_log2).min(self.mi_cols);
            if self.cols > 1 {
                self.min_inner_width = i64::from(width);
            }
        } else {
            let mut max_area = sb_rows * sb_cols;
            let mut widest = 1;
            let mut narrowest = 65536;
            self.log2_cols = tile_log2(1, self.cols);
            for (i, w) in self.col_start_sb.windows(2).enumerate() {
                let (a, b) = (w.first().copied().unwrap_or(0), w.get(1).copied().unwrap_or(0));
                let size = b.saturating_sub(a);
                widest = widest.max(size);
                if (i as u32) + 1 < self.cols {
                    narrowest = narrowest.min(size);
                }
            }
            if self.min_log2 > 0 {
                max_area >>= self.min_log2 + 1;
            }
            self.max_height_sb = (max_area / widest).max(1);
            if self.cols > 1 {
                self.min_inner_width = i64::from(narrowest << self.mib_size_log2);
            }
        }
    }

    fn calculate_rows(&mut self) {
        let sb_rows = align_pow2(self.mi_rows, self.mib_size_log2) >> self.mib_size_log2;
        if self.uniform {
            let seq_mib = self.mib_size_log2 + self.scale_sb;
            let seq_sb_rows = align_pow2(self.mi_rows, seq_mib) >> seq_mib;
            let full = self.mi_rows >> seq_mib;
            let base = full >> self.log2_rows;
            let mut extra = full - (base << self.log2_rows);
            if base == 0 {
                extra += seq_sb_rows - full;
            }
            self.row_start_sb.clear();
            let mut start = 0;
            while start < seq_sb_rows && (self.row_start_sb.len() as u64) < (1u64 << self.log2_rows) {
                self.row_start_sb.push(start << self.scale_sb);
                start += base + u32::from(extra > 0);
                extra = extra.saturating_sub(1);
            }
            self.rows = self.row_start_sb.len() as u32;
            self.log2_rows = tile_log2(1, self.rows);
            self.row_start_sb.push(sb_rows);
        } else {
            self.log2_rows = tile_log2(1, self.rows);
        }
    }

    /// Pixel extent of each tile column (or row), clipped to the frame, as
    /// av2_get_tile_rect gives it for luma.
    fn extents(start_sb: &[u32], mib_size_log2: u32, mi_limit: u32) -> Vec<u32> {
        start_sb
            .windows(2)
            .map(|w| {
                let a = w.first().copied().unwrap_or(0) << mib_size_log2;
                let b = (w.get(1).copied().unwrap_or(0) << mib_size_log2).min(mi_limit);
                b.saturating_sub(a) * 4
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Segmentation {
    pub enabled: bool,
    pub update_map: bool,
    pub update_data: bool,
    pub temporal_update: bool,
    pub feature_mask: [u8; MAX_SEGMENTS],
    pub feature_data: [[i32; SEG_LVL_MAX]; MAX_SEGMENTS],
    pub segid_preskip: bool,
    pub last_active_segid: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Quant {
    pub base_qindex: u32,
    pub y_dc_delta_q: i32,
    pub u_dc_delta_q: i32,
    pub u_ac_delta_q: i32,
    pub v_dc_delta_q: i32,
    pub v_ac_delta_q: i32,
    pub using_qmatrix: bool,
    pub pic_qm_num: u32,
    pub qm_y: [u32; 4],
    pub qm_u: [u32; 4],
    pub qm_v: [u32; 4],
    /// Per segment; read only for segments that use a QM.
    pub qm_index: [u32; MAX_SEGMENTS],
    pub delta_q_present: bool,
    pub delta_q_res: u32,
    pub qindex: [u32; MAX_SEGMENTS],
    pub lossless: [bool; MAX_SEGMENTS],
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Deblock {
    pub apply: [bool; 2],
    pub apply_u: bool,
    pub apply_v: bool,
    pub delta_q_luma: [i32; 2],
    pub delta_q_u: i32,
    pub delta_q_v: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Gdf {
    pub mode: u32,
    pub pic_qp_idx: u32,
    pub pic_scale_idx: u32,
    pub block_size: u32,
    pub block_num: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cdef {
    pub enable: bool,
    pub damping: u32,
    pub on_skip_txfm: bool,
    pub strengths: Vec<u32>,
    pub uv_strengths: Vec<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CcsoPlane {
    pub enable: bool,
    pub bo_only: bool,
    pub scale_idx: u32,
    pub quant_idx: u32,
    pub ext_filter_support: u32,
    pub edge_clf: u32,
    pub max_band_log2: u32,
    /// In read order: d0, then d1, then band.
    pub offsets: Vec<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ccso {
    pub frame_flag: bool,
    pub blk_size_log2: u32,
    pub planes: Vec<CcsoPlane>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameHeader {
    pub obu_type: u8,
    pub cur_mfh_id: u32,
    pub seq_header_id: u32,
    pub refresh_frame_flags: u32,
    /// Frame superblock size: the sequence's, with 256 lowered to 128 on an
    /// intra frame (av2_set_frame_sb_size).
    pub sb_size: u32,
    pub width: u32,
    pub height: u32,
    pub mi_cols: u32,
    pub mi_rows: u32,
    pub allow_screen_content_tools: bool,
    pub cur_frame_force_integer_mv: bool,
    pub allow_intrabc: bool,
    pub allow_global_intrabc: bool,
    pub allow_local_intrabc: bool,
    pub max_bvp_drl_bits: u32,
    pub disable_cdf_update: bool,
    pub tiles: Tiles,
    pub quant: Quant,
    pub seg: Segmentation,
    pub coded_lossless: bool,
    pub tcq_mode: u32,
    pub allow_parity_hiding: bool,
    pub deblock: Deblock,
    pub gdf: Gdf,
    pub cdef: Cdef,
    /// Restoration unit size per plane, set whenever the sequence enables
    /// loop restoration. Frames with a restoration type are refused for now.
    pub restoration_unit_size: [u32; 3],
    pub ccso: Ccso,
    pub tx_mode: u32,
    pub reduced_tx_set_used: u32,
    /// uncompressed_header() length in bits (AVM's uncomp_hdr_size_in_bits).
    pub header_bits: usize,
    pub tile_start: u32,
    pub tile_end: u32,
    /// Bytes from the start of the OBU payload to the first tile's data.
    pub tile_data_offset: usize,
}

impl FrameHeader {
    pub fn parse(seq: &SequenceHeader, obu_type: u8, payload: &[u8]) -> Result<FrameHeader> {
        if obu_type != OBU_CLOSED_LOOP_KEY {
            return Err(Error::Unsupported("frames other than a closed-loop key frame"));
        }
        let r = &mut BitReader::new(payload);
        if !r.bit()? {
            return Err(Error::Invalid("key frame does not start with its first tile group"));
        }
        let start = r.bit_pos();
        let mut h = FrameHeader { obu_type, ..Default::default() };
        h.read_uncompressed_header(seq, r)?;
        h.header_bits = r.bit_pos() - start;
        h.read_tile_indices(r)?;
        r.byte_align()?;
        h.tile_data_offset = r.bytes_read();
        Ok(h)
    }

    fn read_uncompressed_header(&mut self, seq: &SequenceHeader, r: &mut BitReader) -> Result<()> {
        self.cur_mfh_id = r.uvlc()?;
        if self.cur_mfh_id >= MAX_MFH_NUM {
            return Err(Error::Invalid("multi-frame header id"));
        }
        if self.cur_mfh_id != 0 {
            return Err(Error::Unsupported("multi-frame headers"));
        }
        self.seq_header_id = r.uvlc()?;
        if self.seq_header_id >= MAX_SEQ_NUM {
            return Err(Error::Invalid("sequence header id in frame header"));
        }
        if self.seq_header_id != seq.seq_header_id {
            return Err(Error::Invalid("frame names a sequence header that was not sent"));
        }
        if !seq.single_picture_header_flag {
            return Err(Error::Unsupported("sequence header without single_picture_header_flag"));
        }
        // single_picture_header_flag: a shown key frame, every buffer refreshed,
        // no frame size override, no order hint.
        self.sb_size = seq.sb_size.min(128);
        self.refresh_frame_flags = (1 << seq.ref_frames) - 1;
        self.width = seq.max_frame_width;
        self.height = seq.max_frame_height;
        self.mi_cols = align_pow2(self.width, 3) >> 2;
        self.mi_rows = align_pow2(self.height, 3) >> 2;

        // read_screen_content_params, read_intrabc_params
        self.allow_screen_content_tools =
            if seq.force_screen_content_tools == 2 { r.bit()? } else { seq.force_screen_content_tools != 0 };
        self.cur_frame_force_integer_mv = self.allow_screen_content_tools
            && if seq.force_integer_mv == 2 { r.bit()? } else { seq.force_integer_mv != 0 };
        self.allow_intrabc = r.bit()?;
        if self.allow_intrabc {
            self.allow_global_intrabc = r.bit()?;
            self.allow_local_intrabc = !self.allow_global_intrabc || r.bit()?;
            self.max_bvp_drl_bits = seq.def_max_bvp_drl_bits;
            if seq.allow_frame_max_bvp_drl_bits {
                self.max_bvp_drl_bits = r.ref_quniform(3, seq.def_max_bvp_drl_bits - 1)? + 1;
            }
        }

        self.disable_cdf_update = r.bit()?;
        self.read_tile_info(seq, r)?;
        self.read_quantization(seq, r)?;
        self.read_segmentation(seq, r)?;
        self.read_qm_params(seq, r)?;
        let q = &mut self.quant;
        q.delta_q_res = 1;
        q.delta_q_present = q.base_qindex > 0 && r.bit()?;
        if q.delta_q_present {
            q.delta_q_res = 1 << r.f(2)?;
        }
        self.read_lossless_and_qm_index(seq, r)?;

        self.tcq_mode = if self.coded_lossless {
            0
        } else if seq.enable_tcq >= 2 {
            u32::from(r.bit()?)
        } else {
            seq.enable_tcq
        };
        self.allow_parity_hiding =
            !(self.coded_lossless || !seq.enable_parity_hiding || self.tcq_mode != 0) && r.bit()?;

        if !self.coded_lossless {
            self.read_deblock(seq, r)?;
            if seq.enable_gdf {
                self.read_gdf(seq, r)?;
            }
            if seq.enable_cdef {
                self.read_cdef(seq, r)?;
            }
        }
        // all_lossless equals coded_lossless without superres.
        if !self.coded_lossless && seq.enable_restoration {
            self.read_restoration_mode(seq, r)?;
        }
        if !self.coded_lossless && seq.enable_ccso {
            self.read_ccso(seq, r)?;
        }
        self.tx_mode = if self.coded_lossless {
            TX_MODE_ONLY_4X4
        } else if r.bit()? {
            TX_MODE_SELECT
        } else {
            TX_MODE_LARGEST
        };
        // An intra frame: single reference, no skip mode, no BAWP or warp bits.
        self.reduced_tx_set_used = r.f(2)?;
        if seq.film_grain_params_present {
            return Err(Error::Unsupported("film grain"));
        }
        Ok(())
    }

    fn read_tile_info(&mut self, seq: &SequenceHeader, r: &mut BitReader) -> Result<()> {
        let mib = self.sb_size.trailing_zeros() - 2;
        let mut t = Tiles::limits(self.mi_rows, self.mi_cols, mib, seq)?;
        t.read(r)?;
        if t.cols > 1 && (t.min_inner_width << 2) < 64 {
            return Err(Error::Invalid("tile narrower than 64 pixels"));
        }
        if t.cols * t.rows > 1 {
            // A single-picture header sets enable_avg_cdf and avg_cdf_type, so
            // context_update_tile_id is never sent.
            t.tile_size_bytes = r.f(2)? + 1;
        }
        self.tiles = t;
        Ok(())
    }

    fn read_quantization(&mut self, seq: &SequenceHeader, r: &mut BitReader) -> Result<()> {
        fn delta_q(r: &mut BitReader) -> Result<i32> {
            if r.bit()? { r.inv_signed(6) } else { Ok(0) }
        }
        let q = &mut self.quant;
        q.base_qindex = r.f(if seq.bit_depth == 8 { 8 } else { 9 })?;
        q.y_dc_delta_q = if seq.y_dc_delta_q_enabled { delta_q(r)? } else { 0 };
        if seq.num_planes() > 1 && (seq.uv_dc_delta_q_enabled || seq.uv_ac_delta_q_enabled) {
            let diff_uv_delta = seq.separate_uv_delta_q && r.bit()?;
            if !seq.equal_ac_dc_q {
                q.u_dc_delta_q = if seq.uv_dc_delta_q_enabled { delta_q(r)? } else { 0 };
            }
            q.u_ac_delta_q = if seq.uv_ac_delta_q_enabled { delta_q(r)? } else { 0 };
            if seq.equal_ac_dc_q {
                q.u_dc_delta_q = q.u_ac_delta_q;
            }
            if diff_uv_delta {
                if !seq.equal_ac_dc_q {
                    q.v_dc_delta_q = if seq.uv_dc_delta_q_enabled { delta_q(r)? } else { 0 };
                }
                q.v_ac_delta_q = if seq.uv_ac_delta_q_enabled { delta_q(r)? } else { 0 };
                if seq.equal_ac_dc_q {
                    q.v_dc_delta_q = q.v_ac_delta_q;
                }
            } else {
                q.v_dc_delta_q = q.u_dc_delta_q;
                q.v_ac_delta_q = q.u_ac_delta_q;
            }
        }
        Ok(())
    }

    fn read_segmentation(&mut self, seq: &SequenceHeader, r: &mut BitReader) -> Result<()> {
        let s = &mut self.seg;
        s.enabled = r.bit()?;
        if !s.enabled {
            return Ok(());
        }
        let n = if seq.enable_ext_seg { MAX_SEGMENTS } else { MAX_SEGMENTS_8 };
        // find_effective_seg_params: the sequence's table, reusable when the
        // segment count agrees.
        let reuse = seq.seq_seg_info_present_flag
            && seq.seg.enable_ext_seg == seq.enable_ext_seg
            && (!seq.seg.allow_seg_info_change || r.bit()?);
        if reuse {
            s.feature_mask = seq.seg.feature_mask;
            s.feature_data = seq.seg.feature_data;
            (s.segid_preskip, s.last_active_segid) = (seq.seg.segid_preskip, seq.seg.last_active_segid);
        } else {
            seq::read_seg_features(r, n, &mut s.feature_mask, &mut s.feature_data)?;
            (s.segid_preskip, s.last_active_segid) = seq::segdata(&s.feature_mask, n);
        }
        // A key frame has no primary reference: the map and data are coded.
        s.update_map = true;
        s.temporal_update = false;
        s.update_data = true;
        Ok(())
    }

    fn read_qm_params(&mut self, seq: &SequenceHeader, r: &mut BitReader) -> Result<()> {
        let q = &mut self.quant;
        q.using_qmatrix = r.bit()?;
        if !q.using_qmatrix {
            return Ok(());
        }
        q.pic_qm_num = if self.seg.enabled { r.f(2)? + 1 } else { 1 };
        for i in 0..q.pic_qm_num as usize {
            let y = r.f(QM_LEVEL_BITS)?;
            let (u, v) = if seq.num_planes() > 1 {
                if r.bit()? {
                    (y, y)
                } else {
                    let u = r.f(QM_LEVEL_BITS)?;
                    (u, if seq.separate_uv_delta_q { r.f(QM_LEVEL_BITS)? } else { u })
                }
            } else {
                (0, 0)
            };
            for (slot, val) in [(&mut q.qm_y, y), (&mut q.qm_u, u), (&mut q.qm_v, v)] {
                *slot.get_mut(i).ok_or(Error::Invalid("pic_qm_num"))? = val;
            }
        }
        Ok(())
    }

    /// The per-segment loop after delta_q: qindex, lossless, qm_index.
    fn read_lossless_and_qm_index(&mut self, seq: &SequenceHeader, r: &mut BitReader) -> Result<()> {
        let n = if seq.enable_ext_seg { MAX_SEGMENTS } else { MAX_SEGMENTS_8 };
        let q = &mut self.quant;
        let qm_index_bits = 32 - q.pic_qm_num.saturating_sub(1).leading_zeros(); // avm_ceil_log2
        for i in 0..n {
            let mask = self.seg.feature_mask.get(i).copied().unwrap_or(0);
            let qindex = if self.seg.enabled && mask & (1 << SEG_LVL_ALT_Q) != 0 {
                let data = self.seg.feature_data.get(i).and_then(|d| d.get(SEG_LVL_ALT_Q)).copied().unwrap_or(0);
                (q.base_qindex as i32 + data).clamp(0, max_qindex(seq.bit_depth)) as u32
            } else {
                q.base_qindex
            };
            let lossless = qindex == 0
                && !q.delta_q_present
                && q.y_dc_delta_q + seq.base_y_dc_delta_q <= 0
                && q.u_dc_delta_q + seq.base_uv_dc_delta_q <= 0
                && q.v_dc_delta_q + seq.base_uv_dc_delta_q <= 0
                && q.u_ac_delta_q + seq.base_uv_ac_delta_q <= 0
                && q.v_ac_delta_q + seq.base_uv_ac_delta_q <= 0;
            if q.using_qmatrix && !lossless {
                let idx = if qm_index_bits > 0 { r.f(qm_index_bits)? } else { 0 };
                if idx >= q.pic_qm_num {
                    return Err(Error::Invalid("qm_index past pic_qm_num"));
                }
                *q.qm_index.get_mut(i).ok_or(Error::Invalid("segment"))? = idx;
            }
            *q.qindex.get_mut(i).ok_or(Error::Invalid("segment"))? = qindex;
            *q.lossless.get_mut(i).ok_or(Error::Invalid("segment"))? = lossless;
        }
        self.coded_lossless =
            if self.seg.enabled { q.lossless.iter().take(n).all(|&l| l) } else { q.lossless.first() == Some(&true) };
        if self.coded_lossless {
            return Err(Error::Unsupported("lossless frames"));
        }
        Ok(())
    }

    fn read_deblock(&mut self, seq: &SequenceHeader, r: &mut BitReader) -> Result<()> {
        let d = &mut self.deblock;
        d.apply = [r.bit()?, r.bit()?];
        if seq.num_planes() > 1 && (d.apply[0] || d.apply[1]) {
            d.apply_u = r.bit()?;
            d.apply_v = r.bit()?;
        }
        let bits = seq.df_par_bits_minus2 + 2;
        let offset = 1i32 << (bits - 1);
        let delta = |on: bool, r: &mut BitReader| -> Result<Option<i32>> {
            Ok(if on && r.bit()? { Some(r.f(bits)? as i32 - offset) } else { None })
        };
        let luma0 = delta(d.apply[0], r)?.unwrap_or(0);
        d.delta_q_luma[0] = if d.apply[0] { luma0 } else { 0 };
        // The second luma direction inherits the first when not coded.
        d.delta_q_luma[1] = if d.apply[1] { delta(true, r)?.unwrap_or(d.delta_q_luma[0]) } else { 0 };
        d.delta_q_u = delta(d.apply_u, r)?.unwrap_or(0);
        d.delta_q_v = delta(d.apply_v, r)?.unwrap_or(0);
        Ok(())
    }

    /// setup_gdf, with init_gdf's block count.
    fn read_gdf(&mut self, seq: &SequenceHeader, r: &mut BitReader) -> Result<()> {
        let g = &mut self.gdf;
        let t = &self.tiles;
        let mib_size = self.sb_size >> 2;
        g.block_size = if seq.gdf_unit_matches_sb_size { self.sb_size } else { self.sb_size.max(GDF_TEST_BLK_SIZE) };
        if mib_size == 16 && !seq.gdf_unit_matches_sb_size {
            let odd = |s: &[u32]| {
                s.windows(2).take(s.len().saturating_sub(2)).any(|w| w.iter().fold(0, |a, &b| b - a) & 1 != 0)
            };
            if odd(&t.col_start_sb) || odd(&t.row_start_sb) {
                g.block_size = 64;
            }
        }
        let mib_log2 = self.sb_size.trailing_zeros() - 2;
        let blocks = |px: u32| 1 + (px.max(1) - 1) / g.block_size;
        let h: u32 = Tiles::extents(&t.row_start_sb, mib_log2, self.mi_rows).into_iter().map(blocks).sum();
        let w: u32 = Tiles::extents(&t.col_start_sb, mib_log2, self.mi_cols).into_iter().map(blocks).sum();
        g.block_num = h * w;
        g.mode = 1; // single-picture header: GDF is on unless coded lossless
        if g.block_num > 1 {
            g.mode += u32::from(r.bit()?);
        }
        g.pic_qp_idx = r.f(2)?;
        g.pic_scale_idx = r.f(2)?;
        Ok(())
    }

    fn read_cdef(&mut self, seq: &SequenceHeader, r: &mut BitReader) -> Result<()> {
        let c = &mut self.cdef;
        c.enable = true; // single-picture header
        c.damping = r.f(2)? + 3;
        let n = r.f(3)? + 1;
        c.on_skip_txfm = seq.enable_cdef_on_skip_txfm == CDEF_ON_SKIP_TXFM_ADAPTIVE && r.bit()?;
        for _ in 0..n {
            let s = if r.bit()? { r.f(2)? } else { r.f(CDEF_STRENGTH_BITS)? };
            c.strengths.push(s);
            let uv = if seq.num_planes() > 1 { if r.bit()? { r.f(2)? } else { r.f(CDEF_STRENGTH_BITS)? } } else { 0 };
            c.uv_strengths.push(uv);
        }
        Ok(())
    }

    /// decode_restoration_mode, as far as RESTORE_NONE on every plane.
    fn read_restoration_mode(&mut self, seq: &SequenceHeader, r: &mut BitReader) -> Result<()> {
        for p in 0..seq.num_planes() {
            let mut mask = at(&seq.lr_tools_disable_mask, usize::from(p > 0), "plane")?;
            // av2_set_lr_tools
            let tools = (1..3).filter(|i| (mask >> i) & 1 == 0).count() as u32;
            let frame_tools = if tools < 2 {
                mask |= 1 << 3;
                tools + 1
            } else {
                tools + 2
            };
            let _ = mask;
            if r.quniform(frame_tools)? != 0 {
                return Err(Error::Unsupported("loop restoration"));
            }
        }
        let ss = seq.subsampling_x.max(seq.subsampling_y);
        let chroma = RESTORATION_UNITSIZE_MAX >> (3 + ss);
        self.restoration_unit_size = [RESTORATION_UNITSIZE_MAX >> 3, chroma, chroma];
        Ok(())
    }

    fn read_ccso(&mut self, seq: &SequenceHeader, r: &mut BitReader) -> Result<()> {
        let t = &self.tiles;
        let sb_log2 = self.sb_size.trailing_zeros();
        // get_ccso_unit_size_log2_adaptive_tile
        let blk = if seq.ccso_unit_matches_sb_size {
            sb_log2
        } else if t.cols == 1 && t.rows == 1 {
            CCSO_BLK_SIZE_LOG2
        } else if sb_log2 < CCSO_BLK_SIZE_LOG2 {
            let inner = |s: &[u32], m: u32| {
                s.windows(2).take(s.len().saturating_sub(2)).map(|w| w.iter().fold(0, |a, &b| b - a) & m).sum::<u32>()
            };
            let (e2, e4) = (
                inner(&t.col_start_sb, 1) + inner(&t.row_start_sb, 1),
                inner(&t.col_start_sb, 3) + inner(&t.row_start_sb, 3),
            );
            if e4 == 0 {
                (sb_log2 + 2).min(CCSO_BLK_SIZE_LOG2)
            } else if e2 == 0 {
                (sb_log2 + 1).min(CCSO_BLK_SIZE_LOG2)
            } else {
                sb_log2.min(CCSO_BLK_SIZE_LOG2)
            }
        } else {
            sb_log2
        };
        let c = &mut self.ccso;
        c.frame_flag = true; // single-picture header
        c.blk_size_log2 = blk;
        for _ in 0..seq.num_planes() {
            let mut p = CcsoPlane { enable: r.bit()?, ..Default::default() };
            if p.enable {
                // An intra frame never reuses a reference's filter.
                p.bo_only = r.bit()?;
                p.scale_idx = r.f(2)?;
                if p.bo_only {
                    p.max_band_log2 = r.f(3)?;
                    if p.max_band_log2 > CCSO_BAND_NUM_LOG2 {
                        return Err(Error::Invalid("CCSO band count"));
                    }
                } else {
                    p.quant_idx = r.f(2)?;
                    p.ext_filter_support = r.f(3)?;
                    if p.ext_filter_support == 7 {
                        return Err(Error::Invalid("ccso_ext_filter 7"));
                    }
                    let qsz = CCSO_QUANT_SZ
                        .get(p.scale_idx as usize)
                        .and_then(|row| row.get(p.quant_idx as usize))
                        .copied()
                        .unwrap_or(0);
                    p.edge_clf = if qsz != 0 { u32::from(r.bit()?) } else { 0 };
                    p.max_band_log2 = r.f(2)?;
                }
                let intervals = if p.bo_only { 1 } else { at(&CCSO_EDGE_INTERVAL, p.edge_clf as usize, "edge_clf")? };
                let scale = p.scale_idx as i32 + 1; // ccso_scale = {1, 2, 3, 4}
                for _ in 0..(intervals * intervals) << p.max_band_log2 {
                    let mut idx = 0;
                    while idx < 7 && r.bit()? {
                        idx += 1;
                    }
                    p.offsets.push(at(&CCSO_OFFSET, idx, "ccso offset")? * scale);
                }
            }
            c.planes.push(p);
        }
        Ok(())
    }

    /// read_tile_indices_in_tilegroup.
    fn read_tile_indices(&mut self, r: &mut BitReader) -> Result<()> {
        let t = &self.tiles;
        let n = t.cols * t.rows;
        (self.tile_start, self.tile_end) = if n > 1 && r.bit()? {
            let bits = t.log2_rows + t.log2_cols;
            (r.f(bits)?, r.f(bits)?)
        } else {
            (0, n - 1)
        };
        if self.tile_start != 0 {
            return Err(Error::Invalid("first tile group must start at tile 0"));
        }
        if self.tile_end >= n {
            return Err(Error::Invalid("tg_end past the last tile"));
        }
        if self.tile_end != n - 1 {
            return Err(Error::Unsupported("more than one tile group"));
        }
        Ok(())
    }
}
