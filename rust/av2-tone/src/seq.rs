//! sequence_header_obu(), for single-picture streams. AVM: read_sequence_header_obu
//! in av2/decoder/obu.c and av2_read_sequence_header in decodeframe.c. Each
//! group below is one AVM function, read in AVM's order, with the defaults a
//! single-picture header implies where AVM writes them instead of reading.

use crate::bits::BitReader;
use crate::error::{Error, Result};

pub const MAX_SEGMENTS: usize = 16;
pub const MAX_SEGMENTS_8: usize = 8;
pub const SEG_LVL_ALT_Q: usize = 0;
pub const SEG_LVL_SKIP: usize = 1;
pub const SEG_LVL_MAX: usize = 3;
/// MAXQ: 255 + 4 * MAXQ_OFFSET. Only SEG_LVL_ALT_Q carries data.
const SEG_FEATURE_DATA_MAX: [i32; SEG_LVL_MAX] = [255 + 4 * 24, 0, 0];
const SEG_FEATURE_SIGNED: [bool; SEG_LVL_MAX] = [true, false, false];

pub const SEQ_LEVELS: u32 = 22;
pub const SEQ_LEVEL_MAX: u32 = 31;

pub const CDEF_ON_SKIP_TXFM_ADAPTIVE: u32 = 2;
pub const DRL_REORDER_DISABLED: u32 = 0;
pub const DRL_REORDER_CONSTRAINT: u32 = 1;
pub const DRL_REORDER_ALWAYS: u32 = 2;
/// RESTORE_PC_WIENER, which chroma never uses unless told to.
const DEF_UV_LR_TOOLS_DISABLE_MASK: u32 = 1 << 1;
/// RESTORE_SWITCHABLE_TYPES: NONE, PC_WIENER, WIENER_NONSEP.
const RESTORE_SWITCHABLE_TYPES: u32 = 3;
const DELTA_DCQUANT_BITS: u32 = 5;
const DELTA_DCQUANT_MIN: i32 = (1 << (DELTA_DCQUANT_BITS - 2)) - (1 << DELTA_DCQUANT_BITS) + 1;
const MIN_MAX_IBC_DRL_BITS: u32 = 1;
const MAX_MAX_IBC_DRL_BITS: u32 = 3;
const MAX_SEQ_NUM: u32 = 16;

/// Segmentation as a sequence (or multi-frame) header can carry it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SegmentationSyntax {
    pub enable_ext_seg: bool,
    pub allow_seg_info_change: bool,
    pub feature_mask: [u8; MAX_SEGMENTS],
    pub feature_data: [[i32; SEG_LVL_MAX]; MAX_SEGMENTS],
    pub segid_preskip: bool,
    pub last_active_segid: u32,
}

/// Reads one segment-feature table, as read_seg_syntax_info and
/// read_seg_syntax_info_to_segmentation both do.
pub fn read_seg_features(
    r: &mut BitReader,
    max_seg_num: usize,
    mask: &mut [u8; MAX_SEGMENTS],
    data: &mut [[i32; SEG_LVL_MAX]; MAX_SEGMENTS],
) -> Result<()> {
    *mask = [0; MAX_SEGMENTS];
    *data = [[0; SEG_LVL_MAX]; MAX_SEGMENTS];
    for (m, d) in mask.iter_mut().zip(data.iter_mut()).take(max_seg_num) {
        for (j, ((v, &max), &signed)) in
            d.iter_mut().zip(SEG_FEATURE_DATA_MAX.iter()).zip(SEG_FEATURE_SIGNED.iter()).enumerate()
        {
            if r.bit()? {
                *m |= 1 << j;
                let ubits = 32 - (max as u32).leading_zeros(); // get_unsigned_bits
                let raw = if signed { r.inv_signed(ubits)? } else { r.f(ubits)? as i32 };
                *v = raw.clamp(-max, max);
            }
        }
    }
    Ok(())
}

/// av2_calculate_segdata: (segid_preskip, last_active_segid).
pub fn segdata(mask: &[u8; MAX_SEGMENTS], max_seg_num: usize) -> (bool, u32) {
    let mut preskip = false;
    let mut last = 0;
    for (i, &m) in mask.iter().enumerate().take(max_seg_num) {
        for j in 0..SEG_LVL_MAX {
            if m & (1 << j) != 0 {
                preskip |= j >= SEG_LVL_SKIP;
                last = i as u32;
            }
        }
    }
    (preskip, last)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SequenceHeader {
    pub seq_header_id: u32,
    pub profile: u32,
    pub single_picture_header_flag: bool,
    pub level: u32,
    pub tier: u32,
    pub chroma_format_idc: u32,
    pub subsampling_x: u32,
    pub subsampling_y: u32,
    pub monochrome: bool,
    pub bit_depth: u32,
    pub still_picture: bool,
    pub max_tlayer_id: u32,
    pub max_mlayer_id: u32,
    pub monotonic_output_order_flag: bool,
    pub num_bits_width: u32,
    pub num_bits_height: u32,
    pub max_frame_width: u32,
    pub max_frame_height: u32,
    pub conf_win_enabled: bool,
    pub conf_win_left: u32,
    pub conf_win_right: u32,
    pub conf_win_top: u32,
    pub conf_win_bottom: u32,

    // partition group
    /// Superblock size in luma samples: 64, 128 or 256.
    pub sb_size: u32,
    pub enable_sdp: bool,
    pub enable_extended_sdp: bool,
    pub enable_ext_partitions: bool,
    pub enable_uneven_4way_partitions: bool,
    pub max_pb_aspect_ratio_log2_m1: u32,

    // segment group
    pub enable_ext_seg: bool,
    pub seq_seg_info_present_flag: bool,
    pub seg: SegmentationSyntax,

    // intra group
    pub enable_intra_dip: bool,
    pub enable_intra_edge_filter: bool,
    pub enable_mrls: bool,
    pub enable_cfl_intra: bool,
    pub cfl_ds_filter_index: u32,
    pub enable_mhccp: bool,
    pub enable_ibp: bool,

    // inter group (single-picture: nearly all implied)
    pub enable_refmvbank: bool,
    pub enable_drl_reorder: u32,
    pub ref_frames: u32,
    pub def_max_bvp_drl_bits: u32,
    pub allow_frame_max_bvp_drl_bits: bool,
    pub enable_bawp: bool,

    // screen content group
    pub force_screen_content_tools: u32,
    pub force_integer_mv: u32,

    // transform, quantization and entropy group
    pub enable_fsc: bool,
    pub enable_idtx_intra: bool,
    pub enable_ist: bool,
    pub enable_inter_ist: bool,
    pub enable_chroma_dctonly: bool,
    pub reduced_tx_part_set: bool,
    pub enable_cctx: bool,
    pub enable_tcq: u32,
    pub enable_parity_hiding: bool,
    pub enable_avg_cdf: bool,
    pub avg_cdf_type: u32,
    pub separate_uv_delta_q: bool,
    pub equal_ac_dc_q: bool,
    pub base_y_dc_delta_q: i32,
    pub y_dc_delta_q_enabled: bool,
    pub base_uv_dc_delta_q: i32,
    pub uv_dc_delta_q_enabled: bool,
    pub base_uv_ac_delta_q: i32,
    pub uv_ac_delta_q_enabled: bool,

    // filter group
    pub disable_loopfilters_across_tiles: bool,
    pub enable_cdef: bool,
    pub enable_gdf: bool,
    pub gdf_unit_matches_sb_size: bool,
    pub enable_restoration: bool,
    pub lr_tools_disable_mask: [u32; 2],
    pub enable_ccso: bool,
    pub ccso_unit_matches_sb_size: bool,
    pub enable_cdef_on_skip_txfm: u32,
    pub df_par_bits_minus2: u32,

    pub seq_tile_info_present_flag: bool,
    pub film_grain_params_present: bool,
}

impl SequenceHeader {
    pub fn num_planes(&self) -> usize {
        if self.monochrome { 1 } else { 3 }
    }

    /// Superblock size in 4x4 mode-info units, log2.
    pub fn mib_size_log2(&self) -> u32 {
        self.sb_size.trailing_zeros() - 2
    }

    pub fn parse(payload: &[u8]) -> Result<SequenceHeader> {
        let r = &mut BitReader::new(payload);
        let mut s = SequenceHeader { seq_header_id: r.uvlc()?, ..Default::default() };
        if s.seq_header_id >= MAX_SEQ_NUM {
            return Err(Error::Invalid("seq_header_id"));
        }
        s.profile = r.f(5)?;
        s.single_picture_header_flag = r.bit()?;
        if !s.single_picture_header_flag {
            // Layers, timing, decoder models and the multi-frame tools all sit
            // behind this flag. A still from this repo's encoder never sets it.
            return Err(Error::Unsupported("sequence header without single_picture_header_flag"));
        }
        s.level = r.f(5)?;
        if s.level >= SEQ_LEVELS && s.level != SEQ_LEVEL_MAX {
            return Err(Error::Invalid("seq_max_level_idx"));
        }
        s.tier = 0;
        s.chroma_format_idc = r.uvlc()?;
        (s.subsampling_x, s.subsampling_y) = match s.chroma_format_idc {
            0 | 1 => (1, 1), // 4:2:0, and 4:0:0 as AVM records it
            2 => (0, 0),
            3 => (1, 0),
            _ => return Err(Error::Invalid("chroma_format_idc")),
        };
        s.bit_depth = match r.uvlc()? {
            0 => 10,
            1 => 8,
            2 => 12,
            _ => return Err(Error::Invalid("bitdepth_lut_idx")),
        };
        s.monochrome = s.chroma_format_idc == 1;
        s.still_picture = true;
        s.monotonic_output_order_flag = true;

        s.num_bits_width = r.f(4)? + 1;
        s.num_bits_height = r.f(4)? + 1;
        s.max_frame_width = r.f(s.num_bits_width)? + 1;
        s.max_frame_height = r.f(s.num_bits_height)? + 1;
        s.conf_win_enabled = r.bit()?;
        if s.conf_win_enabled {
            s.conf_win_left = r.uvlc()?;
            s.conf_win_right = r.uvlc()?;
            s.conf_win_top = r.uvlc()?;
            s.conf_win_bottom = r.uvlc()?;
            if s.conf_win_left >= s.max_frame_width
                || s.conf_win_right >= s.max_frame_width
                || s.conf_win_top >= s.max_frame_height
                || s.conf_win_bottom >= s.max_frame_height
            {
                return Err(Error::Invalid("conformance window offset past the frame"));
            }
        }

        s.read_partition_group(r)?;
        s.read_segment_group(r)?;
        s.read_intra_group(r)?;
        s.read_inter_group(r)?;
        // Screen content group: a single-picture header leaves both to the frame.
        s.force_screen_content_tools = 2;
        s.force_integer_mv = 2;
        s.read_transform_quant_group(r)?;
        s.read_filter_group(r)?;
        s.seq_tile_info_present_flag = r.bit()?;
        if s.seq_tile_info_present_flag {
            return Err(Error::Unsupported("sequence-level tile info"));
        }
        s.film_grain_params_present = r.bit()?;
        if r.bit()? {
            return Err(Error::Unsupported("sequence header extension"));
        }
        r.trailing_bits()?;
        Ok(s)
    }

    fn read_partition_group(&mut self, r: &mut BitReader) -> Result<()> {
        // setup_seq_sb_size: 1 -> 256, 01 -> 128, 00 -> 64.
        self.sb_size = if r.bit()? {
            256
        } else if r.bit()? {
            128
        } else {
            64
        };
        self.enable_sdp = !self.monochrome && r.bit()?;
        self.enable_extended_sdp = false; // read only when !single_picture_header_flag
        self.enable_ext_partitions = r.bit()?;
        self.enable_uneven_4way_partitions = self.enable_ext_partitions && r.bit()?;
        self.max_pb_aspect_ratio_log2_m1 = if r.bit()? { u32::from(r.bit()?) } else { 2 };
        Ok(())
    }

    fn read_segment_group(&mut self, r: &mut BitReader) -> Result<()> {
        self.enable_ext_seg = r.bit()?;
        self.seq_seg_info_present_flag = r.bit()?;
        if self.seq_seg_info_present_flag {
            let seg = &mut self.seg;
            seg.enable_ext_seg = self.enable_ext_seg;
            seg.allow_seg_info_change = r.bit()?;
            let n = if seg.enable_ext_seg { MAX_SEGMENTS } else { MAX_SEGMENTS_8 };
            read_seg_features(r, n, &mut seg.feature_mask, &mut seg.feature_data)?;
            (seg.segid_preskip, seg.last_active_segid) = segdata(&seg.feature_mask, n);
        }
        Ok(())
    }

    fn read_intra_group(&mut self, r: &mut BitReader) -> Result<()> {
        self.enable_intra_dip = r.bit()?;
        self.enable_intra_edge_filter = r.bit()?;
        self.enable_mrls = r.bit()?;
        self.enable_cfl_intra = r.bit()?;
        self.cfl_ds_filter_index = if self.monochrome { 0 } else { r.f(2)? };
        self.enable_mhccp = r.bit()?;
        self.enable_ibp = r.bit()?;
        Ok(())
    }

    /// read_sequence_inter_group_tool_flags, single-picture branch: three
    /// fields survive, plus the intra block copy DRL bits.
    fn read_inter_group(&mut self, r: &mut BitReader) -> Result<()> {
        self.enable_refmvbank = r.bit()?;
        self.enable_drl_reorder = if r.bit()? {
            DRL_REORDER_DISABLED
        } else if r.bit()? {
            DRL_REORDER_CONSTRAINT
        } else {
            DRL_REORDER_ALWAYS
        };
        self.ref_frames = 2;
        self.def_max_bvp_drl_bits = r.quniform(MAX_MAX_IBC_DRL_BITS - MIN_MAX_IBC_DRL_BITS + 1)? + MIN_MAX_IBC_DRL_BITS;
        self.allow_frame_max_bvp_drl_bits = r.bit()?;
        self.enable_bawp = r.bit()?;
        Ok(())
    }

    fn read_transform_quant_group(&mut self, r: &mut BitReader) -> Result<()> {
        let mono = self.monochrome;
        self.enable_fsc = r.bit()?;
        self.enable_idtx_intra = self.enable_fsc || r.bit()?;
        self.enable_ist = r.bit()?;
        self.enable_inter_ist = r.bit()?;
        self.enable_chroma_dctonly = !mono && r.bit()?;
        self.reduced_tx_part_set = r.bit()?;
        self.enable_cctx = !mono && r.bit()?;
        // A single-picture header has no second TCQ bit: 0 or TCQ_8ST.
        self.enable_tcq = u32::from(r.bit()?);
        self.enable_parity_hiding = if self.enable_tcq == 0 || self.enable_tcq >= 2 { r.bit()? } else { false };
        self.enable_avg_cdf = true;
        self.avg_cdf_type = 1;
        self.separate_uv_delta_q = !mono && r.bit()?;
        self.equal_ac_dc_q = r.bit()?;
        if !self.equal_ac_dc_q {
            self.base_y_dc_delta_q = DELTA_DCQUANT_MIN + r.f(DELTA_DCQUANT_BITS)? as i32;
            self.y_dc_delta_q_enabled = r.bit()?;
        }
        if !mono {
            if !self.equal_ac_dc_q {
                self.base_uv_dc_delta_q = DELTA_DCQUANT_MIN + r.f(DELTA_DCQUANT_BITS)? as i32;
                self.uv_dc_delta_q_enabled = r.bit()?;
            }
            self.base_uv_ac_delta_q = DELTA_DCQUANT_MIN + r.f(DELTA_DCQUANT_BITS)? as i32;
            self.uv_ac_delta_q_enabled = r.bit()?;
            if self.equal_ac_dc_q {
                self.base_uv_dc_delta_q = self.base_uv_ac_delta_q;
            }
        }
        Ok(())
    }

    fn read_filter_group(&mut self, r: &mut BitReader) -> Result<()> {
        self.disable_loopfilters_across_tiles = r.bit()?;
        self.enable_cdef = r.bit()?;
        self.enable_gdf = r.bit()?;
        self.gdf_unit_matches_sb_size = self.enable_gdf && self.sb_size == 64 && r.bit()?;
        self.enable_restoration = r.bit()?;
        if self.enable_restoration {
            let mut luma = 0;
            for i in 1..RESTORE_SWITCHABLE_TYPES {
                luma |= u32::from(r.bit()?) << i;
            }
            let chroma = if r.bit()? {
                let mut m = DEF_UV_LR_TOOLS_DISABLE_MASK;
                for i in 1..RESTORE_SWITCHABLE_TYPES {
                    if DEF_UV_LR_TOOLS_DISABLE_MASK & (1 << i) == 0 {
                        m |= u32::from(r.bit()?) << i;
                    }
                }
                m
            } else {
                luma | DEF_UV_LR_TOOLS_DISABLE_MASK
            };
            self.lr_tools_disable_mask = [luma, chroma];
        }
        self.enable_ccso = r.bit()?;
        self.ccso_unit_matches_sb_size = self.enable_ccso && r.bit()?;
        self.enable_cdef_on_skip_txfm = CDEF_ON_SKIP_TXFM_ADAPTIVE;
        self.df_par_bits_minus2 = r.f(2)?;
        Ok(())
    }
}
