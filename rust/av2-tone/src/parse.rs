//! The tile parse: every symbol of a key frame's tiles, in AVM's order, with
//! no reconstruction. A port of decode_tile, decode_partition_sb,
//! decode_partition and read_partition (av2/decoder/decodeframe.c) and the
//! block-level readers they call. Built up layer by layer against AVM's own
//! symbol trace; whatever isn't ported yet stops the parse with
//! `Error::Unsupported`.

use crate::Headers;
use crate::avm_defs::*;
use crate::blk::*;
use crate::block::Grid;
use crate::cdf::FrameContext;
use crate::cdf_tables::{DO_SPLIT_CDF, DO_SQUARE_SPLIT_CDF, RECT_TYPE_CDF};
use crate::entropy::{SymbolReader, TraceSym};
use crate::error::{Error, Result};
use crate::partition::*;
use crate::residual::EntropyContexts;
use crate::tile::{TileBuffer, tile_buffers};

/// Partition contexts, one per mi column (above) and per mi row within the
/// superblock (left), for the luma or shared tree [0] and the chroma tree [1].
struct PartitionContexts {
    above: [Vec<u8>; 2],
    left: [[u8; MAX_MIB_SIZE]; 2],
}

/// The current tile's bounds, in mi units (TileInfo).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TileRect {
    pub mi_row_start: usize,
    pub mi_row_end: usize,
    pub mi_col_start: usize,
    pub mi_col_end: usize,
}

/// A frame's parse state, shared by its tiles.
pub(crate) struct FrameParse<'h> {
    pub(crate) h: &'h Headers,
    pub(crate) f: PartFrame,
    /// The frame's superblock size (a BLOCK_SIZE).
    pub(crate) sb_size: usize,
    pub(crate) mib_size: usize,
    sdp: bool,
    enable_cfl: bool,
    enable_mhccp: bool,
    pctx: PartitionContexts,
    pub(crate) trees: PTrees,
    pub(crate) tree_type: usize,
    /// xd->is_cfl_allowed_in_sdp.
    pub(crate) cfl_allowed_in_sdp: usize,
    pub(crate) grid: Grid,
    /// cur_frame->seg_map, mi_cols wide.
    pub(crate) seg_map: Vec<u8>,
    /// xd->lossless, per segment.
    pub(crate) lossless: [bool; MAX_SEGMENTS],
    pub(crate) has_lossless_segment: bool,
    /// xd->cdef_transmitted.
    pub(crate) cdef_transmitted: [bool; 16],
    pub(crate) tile: TileRect,
    /// above_entropy_context and left_entropy_context.
    pub(crate) ec: EntropyContexts,
    /// mi_params->tx_type_map: each luma transform block's type, at its
    /// top-left 4x4 (and every 16x16 of an inter block's).
    pub(crate) tx_type_map: Vec<u16>,
    /// mi_params->cctx_type_map: each chroma transform's cross-chroma type,
    /// over the luma 4x4s it covers.
    pub(crate) cctx_type_map: Vec<u8>,
    /// xd->eob_u_flag: whether the last U block coded any coefficient.
    pub(crate) eob_u_flag: bool,
}

/// One tile's reader and adapting CDFs.
pub(crate) struct TileState<'a> {
    pub r: SymbolReader<'a>,
    pub fc: FrameContext,
}

impl<'h> FrameParse<'h> {
    pub fn new(h: &'h Headers) -> Result<FrameParse<'h>> {
        let s = &h.seq;
        let fr = &h.frame;
        let sb_size = match fr.sb_size {
            64 => BLOCK_64X64,
            128 => BLOCK_128X128,
            _ => return Err(Error::Invalid("frame superblock size")),
        };
        let mib_size = mi_w(sb_size)?;
        let f = PartFrame {
            mi_rows: fr.mi_rows as usize,
            mi_cols: fr.mi_cols as usize,
            ss_x: s.subsampling_x as usize,
            ss_y: s.subsampling_y as usize,
            monochrome: s.monochrome,
            // is_sdp_enabled_in_keyframe
            sdp: !s.monochrome && s.enable_sdp,
            enable_ext_partitions: s.enable_ext_partitions,
            enable_uneven_4way: s.enable_uneven_4way_partitions,
            max_pb_aspect_ratio_log2_m1: s.max_pb_aspect_ratio_log2_m1 as usize,
        };
        let cols = f.mi_cols.div_ceil(mib_size) * mib_size + MAX_MIB_SIZE;
        if fr.quant.delta_q_present {
            return Err(Error::Unsupported("delta q"));
        }
        let mut lossless = [false; MAX_SEGMENTS];
        lossless.copy_from_slice(&fr.quant.lossless);
        let grid = Grid::new(f.mi_rows, f.mi_cols);
        let maps = grid.stride * grid.rows;
        Ok(FrameParse {
            h,
            sdp: f.sdp,
            f,
            sb_size,
            mib_size,
            enable_cfl: s.enable_cfl_intra,
            enable_mhccp: s.enable_mhccp,
            pctx: PartitionContexts { above: [vec![0; cols], vec![0; cols]], left: [[0; MAX_MIB_SIZE]; 2] },
            trees: PTrees::default(),
            tree_type: SHARED_PART,
            cfl_allowed_in_sdp: 0,
            ec: EntropyContexts::new(grid.stride + MAX_MIB_SIZE),
            tx_type_map: vec![0; maps],
            cctx_type_map: vec![0; maps],
            eob_u_flag: false,
            grid,
            seg_map: vec![0; f.mi_rows * f.mi_cols],
            has_lossless_segment: lossless.iter().any(|&l| l),
            lossless,
            cdef_transmitted: [false; 16],
            tile: TileRect::default(),
        })
    }

    /// decode_tile: every superblock of one tile, in raster order.
    fn decode_tile(&mut self, t: &mut TileState, b: &TileBuffer) -> Result<()> {
        let tiles = &self.h.frame.tiles;
        let mib_log2 = mi_w_log2(self.sb_size)?;
        let sb_row = |i: u32| (i as usize) << mib_log2;
        let row0 = sb_row(*tiles.row_start_sb.at(b.row as usize)?);
        let row1 = sb_row(*tiles.row_start_sb.at(b.row as usize + 1)?).min(self.f.mi_rows);
        let col0 = sb_row(*tiles.col_start_sb.at(b.col as usize)?);
        let col1 = sb_row(*tiles.col_start_sb.at(b.col as usize + 1)?).min(self.f.mi_cols);
        self.tile = TileRect { mi_row_start: row0, mi_row_end: row1, mi_col_start: col0, mi_col_end: col1 };
        // av2_zero_above_context: this tile's columns, aligned to superblocks.
        let width = (col1 - col0).div_ceil(self.mib_size) * self.mib_size;
        for a in &mut self.pctx.above {
            a.get_mut(col0..col0 + width).ok_or(Error::Invalid("tile columns"))?.fill(0);
        }
        for (plane, a) in self.ec.above.iter_mut().enumerate() {
            let s = if plane == 0 { 0 } else { self.f.ss_x };
            a.get_mut(col0 >> s..(col0 + width) >> s).ok_or(Error::Invalid("tile columns"))?.fill(0);
        }
        let mut mi_row = row0;
        while mi_row < row1 {
            // av2_zero_left_context
            self.pctx.left = [[0; MAX_MIB_SIZE]; 2];
            self.ec.left = [[0; MAX_MIB_SIZE]; 3];
            let mut mi_col = col0;
            while mi_col < col1 {
                self.decode_partition_sb(t, mi_row, mi_col)?;
                if t.r.has_overflowed() {
                    return Err(Error::Truncated("tile data"));
                }
                mi_col += self.mib_size;
            }
            mi_row += self.mib_size;
        }
        Ok(())
    }

    /// decode_partition_sb.
    fn decode_partition_sb(&mut self, t: &mut TileState, mi_row: usize, mi_col: usize) -> Result<()> {
        self.tree_type = SHARED_PART;
        self.trees = PTrees::default();
        let root = self.trees.alloc(None, 0);
        let chroma_root = if self.sdp { Some(self.trees.alloc(None, 0)) } else { None };
        self.decode_partition(t, mi_row, mi_col, self.sb_size, root, chroma_root)
    }

    /// decode_partition (parse half). `node` is this tree's node; with SDP,
    /// `other` is the matching node of the other tree (AVM's ptree_luma).
    fn decode_partition(
        &mut self,
        t: &mut TileState,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
        node: usize,
        other: Option<usize>,
    ) -> Result<()> {
        if mi_row >= self.f.mi_rows || mi_col >= self.f.mi_cols {
            return Ok(()); // av2_mark_block_as_pseudo_coded; nothing is read
        }
        if self.sdp && bsize == BLOCK_64X64 && self.tree_type == SHARED_PART {
            let other = other.ok_or(Error::Invalid("SDP chroma tree"))?;
            self.tree_type = LUMA_PART;
            self.decode_partition(t, mi_row, mi_col, bsize, node, Some(other))?;
            self.tree_type = CHROMA_PART;
            self.decode_partition(t, mi_row, mi_col, bsize, other, Some(node))?;
            self.tree_type = SHARED_PART;
            return Ok(());
        }

        let parent = self.trees.get(node)?.parent;
        if let Some(p) = parent {
            let pn = self.trees.get(p)?;
            let info = if pn.bsize == self.sb_size {
                match pn.partition {
                    PARTITION_VERT => SB_VERT_PARTITION,
                    PARTITION_HORZ | PARTITION_SPLIT => SB_HORZ_OR_QUAD_PARTITION,
                    _ => INVALID_INTRABC_SB_PARTITION,
                }
            } else {
                pn.sb_root_partition_info
            };
            self.trees.get_mut(node)?.sb_root_partition_info = info;
        }
        let is_sb_root = bsize == self.sb_size;
        if is_sb_root {
            self.cfl_allowed_in_sdp = self.cfl_for_sdp(other, PARTITION_NONE, bsize)?;
            let n = self.trees.get_mut(node)?;
            n.is_cfl_allowed = CFL_DISALLOWED_FOR_CHROMA;
            n.region_type = INTRA_REGION;
            n.extended_sdp_allowed = false;
        }

        {
            let n = self.trees.get_mut(node)?;
            n.bsize = bsize;
            n.mi_row = mi_row;
            n.mi_col = mi_col;
            n.is_settled = true;
        }
        if self.sdp && self.tree_type == SHARED_PART {
            let o = other.ok_or(Error::Invalid("SDP chroma tree"))?;
            let n = self.trees.get_mut(o)?;
            n.bsize = bsize;
            n.mi_row = mi_row;
            n.mi_col = mi_col;
            n.is_settled = true;
            n.is_cfl_allowed = CFL_DISALLOWED_FOR_CHROMA;
        }
        let (parent_info, parent_bsize, parent_partition) = match parent {
            Some(p) => {
                let pn = self.trees.get(p)?;
                (Some(pn.chroma_ref), pn.bsize, pn.partition)
            }
            None => (None, BLOCK_INVALID, PARTITION_NONE),
        };
        let index = self.trees.get(node)?.index;
        let info = chroma_ref_info(
            self.tree_type,
            mi_row,
            mi_col,
            index,
            bsize,
            parent_info.as_ref(),
            parent_bsize,
            parent_partition,
            self.f.ss_x,
            self.f.ss_y,
        )?;
        self.trees.get_mut(node)?.chroma_ref = info;

        let partition = if is_partition_point(bsize) {
            self.read_partition(t, mi_row, mi_col, node, other, bsize)?
        } else {
            PARTITION_NONE
        };
        let cfl = self.cfl_for_sdp(other, partition, bsize)?;
        let cfl_here = {
            let n = self.trees.get_mut(node)?;
            n.is_cfl_allowed |= cfl;
            n.partition = partition;
            n.is_cfl_allowed
        };
        if !is_sb_root && parent.is_some() {
            // An intra frame: no extended SDP, every region intra.
            let n = self.trees.get_mut(node)?;
            n.extended_sdp_allowed = false;
            n.region_type = INTRA_REGION;
        }

        let nsub = match partition {
            PARTITION_HORZ_4A | PARTITION_HORZ_4B | PARTITION_VERT_4A | PARTITION_VERT_4B | PARTITION_SPLIT
            | PARTITION_HORZ_3 | PARTITION_VERT_3 => 4,
            PARTITION_HORZ | PARTITION_VERT => 2,
            _ => 0,
        };
        for i in 0..nsub {
            let c = self.trees.alloc(Some(node), i);
            *self.trees.get_mut(node)?.sub.get_mut(i).ok_or(Error::Invalid("subtree"))? = Some(c);
            self.trees.get_mut(c)?.is_cfl_allowed = cfl_here;
            if self.sdp && self.tree_type == SHARED_PART {
                let o = other.ok_or(Error::Invalid("SDP chroma tree"))?;
                let oc = self.trees.alloc(Some(o), i);
                *self.trees.get_mut(o)?.sub.get_mut(i).ok_or(Error::Invalid("subtree"))? = Some(oc);
            }
        }
        if partition == PARTITION_NONE {
            self.cfl_allowed_in_sdp = cfl_here;
        }

        let subsize = partition_subsize(bsize, partition)?;
        if subsize == BLOCK_INVALID {
            return Err(Error::Invalid("partition invalid for the block size"));
        }
        let mut test_subsize = subsize;
        if self.tree_type == SHARED_PART {
            let idx = usize::from(partition == PARTITION_HORZ || partition == PARTITION_VERT);
            let ci = chroma_ref_info(
                self.tree_type,
                mi_row,
                mi_col,
                idx,
                subsize,
                Some(&info),
                bsize,
                partition,
                self.f.ss_x,
                self.f.ss_y,
            )?;
            test_subsize = ci.bsize_base;
        }
        if self.tree_type != LUMA_PART && plane_bsize(test_subsize, self.f.ss_x, self.f.ss_y)? == BLOCK_INVALID {
            return Err(Error::Invalid("block size invalid with this subsampling"));
        }
        if !chroma_ref_within_boundary(&self.f, self.tree_type, info.is_chroma_ref, mi_row, mi_col, bsize, partition)? {
            return Err(Error::Invalid("chroma block outside the frame"));
        }

        let child = |s: &Self, i: usize| -> Result<(usize, Option<usize>)> {
            let c = s.trees.child(Some(node), i)?.ok_or(Error::Invalid("subtree"))?;
            Ok((c, s.trees.child(other, i)?))
        };
        let (hbs_w, hbs_h) = (mi_w(bsize)? / 2, mi_h(bsize)? / 2);
        let (ebs_w, ebs_h) = (mi_w(bsize)? / 8, mi_h(bsize)? / 8);
        match partition {
            PARTITION_NONE => self.decode_block(t, mi_row, mi_col, partition, subsize, node, 0)?,
            PARTITION_HORZ | PARTITION_VERT => {
                for i in 0..2 {
                    let (c, o) = child(self, i)?;
                    let (r, cc) = if partition == PARTITION_HORZ {
                        (mi_row + i * hbs_h, mi_col)
                    } else {
                        (mi_row, mi_col + i * hbs_w)
                    };
                    self.decode_partition(t, r, cc, subsize, c, o)?;
                }
            }
            PARTITION_HORZ_4A | PARTITION_HORZ_4B | PARTITION_VERT_4A | PARTITION_VERT_4B => {
                let horz = partition == PARTITION_HORZ_4A || partition == PARTITION_HORZ_4B;
                let big = partition_subsize(bsize, if horz { PARTITION_HORZ } else { PARTITION_VERT })?;
                let med = partition_subsize(big, if horz { PARTITION_HORZ } else { PARTITION_VERT })?;
                let a = partition == PARTITION_HORZ_4A || partition == PARTITION_VERT_4A;
                let order = if a {
                    [(subsize, 1), (med, 2), (big, 4), (subsize, 1)]
                } else {
                    [(subsize, 1), (big, 4), (med, 2), (subsize, 1)]
                };
                let (step, mut pos) = if horz { (ebs_h, mi_row) } else { (ebs_w, mi_col) };
                for (i, (sz, n)) in order.into_iter().enumerate() {
                    let (c, o) = child(self, i)?;
                    let (r, cc) = if horz { (pos, mi_col) } else { (mi_row, pos) };
                    self.decode_partition(t, r, cc, sz, c, o)?;
                    pos += n * step;
                }
            }
            PARTITION_HORZ_3 | PARTITION_VERT_3 => {
                for i in 0..4 {
                    let (sz, r, cc) = h_partition(bsize, i, partition)?;
                    let (c, o) = child(self, i)?;
                    self.decode_partition(t, mi_row + r, mi_col + cc, sz, c, o)?;
                }
            }
            PARTITION_SPLIT => {
                for i in 0..4 {
                    let (c, o) = child(self, i)?;
                    self.decode_partition(t, mi_row + (i >> 1) * hbs_h, mi_col + (i & 1) * hbs_w, subsize, c, o)?;
                }
            }
            _ => return Err(Error::Invalid("partition type")),
        }

        // update_ext_partition_context
        if partition == PARTITION_NONE {
            self.update_partition_context(mi_row, mi_col, subsize, bsize)?;
            if self.sdp && self.tree_type == SHARED_PART {
                self.tree_type = CHROMA_PART;
                self.update_partition_context(mi_row, mi_col, subsize, bsize)?;
                self.tree_type = SHARED_PART;
            }
        }
        Ok(())
    }

    fn cfl_for_sdp(&self, luma: Option<usize>, current: usize, bsize: usize) -> Result<usize> {
        cfl_allowed_for_sdp(&self.trees, self.enable_cfl, self.enable_mhccp, self.tree_type, luma, current, bsize)
    }

    /// update_partition_context.
    fn update_partition_context(&mut self, mi_row: usize, mi_col: usize, subsize: usize, bsize: usize) -> Result<()> {
        let plane = usize::from(self.tree_type == CHROMA_PART);
        let above = *PARTITION_CONTEXT_ABOVE.at(subsize)?;
        let left = *PARTITION_CONTEXT_LEFT.at(subsize)?;
        let (w, h) = (mi_w(bsize)?, mi_h(bsize)?);
        let lr = mi_row & MAX_MIB_MASK;
        self.pctx
            .above
            .at_mut(plane)?
            .get_mut(mi_col..mi_col + w)
            .ok_or(Error::Invalid("partition context"))?
            .fill(above);
        self.pctx.left.at_mut(plane)?.get_mut(lr..lr + h).ok_or(Error::Invalid("partition context"))?.fill(left);
        Ok(())
    }

    /// read_partition.
    fn read_partition(
        &mut self,
        t: &mut TileState,
        mi_row: usize,
        mi_col: usize,
        node: usize,
        luma: Option<usize>,
        bsize: usize,
    ) -> Result<usize> {
        let plane = usize::from(self.tree_type == CHROMA_PART);
        let derived = forced_partition(&self.f, &self.trees, self.tree_type, mi_row, mi_col, bsize, luma)?;
        let info = self.trees.get(node)?.chroma_ref;
        let allowed = allowed_partitions(&self.f, self.sb_size, self.tree_type, mi_row, mi_col, bsize, Some(&info))?;
        if derived != PARTITION_INVALID && *allowed.at(derived)? {
            return Ok(derived);
        }
        let only = only_allowed(&allowed);
        if only != PARTITION_INVALID {
            return Ok(only);
        }
        let ctx = |s: &Self, rect: usize, mode: usize| -> Result<usize> {
            let a = s.pctx.above.at(plane)?;
            let l = s.pctx.left.at(plane)?;
            let lr = mi_row & MAX_MIB_MASK;
            plane_context(
                *a.at(mi_col)?,
                *l.at(lr)?,
                a.get(mi_col + mi_w(bsize)? / 2).copied().unwrap_or(0),
                l.get(lr + mi_h(bsize)? / 2).copied().unwrap_or(0),
                bsize,
                rect,
                mode,
            )
        };
        let do_split = if !allowed[PARTITION_NONE] {
            true
        } else {
            let c = ctx(self, 0, SPLIT_CTX_MODE)?;
            t.fc.read(&mut t.r, &DO_SPLIT_CDF, [plane, c])? == 1
        };
        if !do_split {
            return Ok(PARTITION_NONE);
        }
        if allowed[PARTITION_SPLIT] {
            let c = ctx(self, 0, SQUARE_SPLIT_CTX_MODE)?;
            if t.fc.read(&mut t.r, &DO_SQUARE_SPLIT_CDF, [plane, c])? == 1 {
                return Ok(PARTITION_SPLIT);
            }
        }
        let mut rect = rect_type_implied(bsize, self.tree_type);
        if rect == RECT_INVALID {
            rect = only_allowed_rect(&allowed);
        }
        if rect == RECT_INVALID {
            let c = ctx(self, 0, RECT_TYPE_CTX_MODE)?;
            rect = t.fc.read(&mut t.r, &RECT_TYPE_CDF, [plane, c])?;
        }
        let do_ext = match ext_implied(&allowed, rect) {
            Some(v) => v,
            None => return Err(Error::Unsupported("signalled extended partitions")),
        };
        let mut do_4way = false;
        let mut b_type = false;
        if do_ext {
            do_4way = match uneven_implied(&allowed, rect) {
                Some(v) => v,
                None => return Err(Error::Unsupported("signalled uneven 4-way partitions")),
            };
            if do_4way {
                b_type = t.r.read_bit()?;
            }
        }
        Ok(rect_partition(do_ext, do_4way, b_type, rect))
    }

    /// parse_decode_block: one coding block's mode info, palette tokens,
    /// transform sizes and coefficients.
    #[allow(clippy::too_many_arguments)]
    fn decode_block(
        &mut self,
        t: &mut TileState,
        mi_row: usize,
        mi_col: usize,
        partition: usize,
        bsize: usize,
        parent: usize,
        index: usize,
    ) -> Result<()> {
        let max_ar = 1usize << (self.h.seq.max_pb_aspect_ratio_log2_m1 + 1);
        let (w, hh) = (bw(bsize)?, bh(bsize)?);
        if w > hh * max_ar || hh > w * max_ar {
            return Err(Error::Invalid("block violates the aspect-ratio limit"));
        }
        // decode_mbmi_block
        t.r.set_trace_context(mi_col as i32, mi_row as i32, self.tree_type as u8);
        let xd = self.set_offsets(mi_row, mi_col, bsize, index, parent)?;
        {
            let pn = self.trees.get(parent)?;
            let (region, sb_root) = (pn.region_type, pn.sb_root_partition_info);
            let tree = self.tree_type;
            let m = self.grid.mi_mut(xd.cur)?;
            m.partition = partition;
            m.region_type = region;
            m.tree_type = tree;
            m.sb_root_partition_info = sb_root;
        }
        self.read_mode_info(t, &xd)?;
        self.read_palette_tokens(t, &xd)?;
        self.read_residual(t, &xd)
    }
}

/// Parses every tile of the key frame, in AVM's order. With `trace`, records
/// each symbol as AVM's accounting build would, carrying the block context
/// across tiles as AVM's does.
pub(crate) fn parse_tiles(h: &Headers, data: &[u8], mut trace: Option<&mut Vec<TraceSym>>) -> Result<()> {
    let mut fp = FrameParse::new(h)?;
    let fc = FrameContext::default_for(h.frame.quant.base_qindex, h.seq.reduced_tx_part_set)?;
    let allow_update = !h.frame.disable_cdf_update;
    // AVM's accounting starts at mi (-1, -1), before any block sets it.
    let mut ctx = (-1, -1, 0);
    for b in tile_buffers(h, data)? {
        let mut t = TileState { r: SymbolReader::new(b.data, allow_update), fc: fc.clone() };
        if trace.is_some() {
            t.r.enable_trace();
            t.r.set_trace_context(ctx.0, ctx.1, ctx.2);
        }
        let r = fp.decode_tile(&mut t, &b);
        if let Some(out) = trace.as_deref_mut() {
            out.extend(t.r.take_trace());
            ctx = t.r.trace_context().unwrap_or(ctx);
        }
        r?;
    }
    Ok(())
}

/// What a traced parse produced: AVM-shaped symbol records for every tile, in
/// decode order, and how the parse ended.
pub struct ParseTrace {
    pub syms: Vec<TraceSym>,
    pub result: Result<()>,
}

/// Parses every tile of the key frame in an OBU stream, recording each symbol
/// as AVM's accounting build would.
pub fn trace_obus(stream: &[u8]) -> ParseTrace {
    let mut syms = Vec::new();
    let result = crate::parse_obus(stream).and_then(|(h, group)| parse_tiles(&h, group.data, Some(&mut syms)));
    ParseTrace { syms, result }
}

/// av2trace --digest's FNV-1a over a trace: each symbol's mi column and row,
/// value, tree, coding mode and cost, little-endian.
pub fn trace_digest(syms: &[TraceSym]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for s in syms {
        eat(&s.x.to_le_bytes());
        eat(&s.y.to_le_bytes());
        eat(&s.value.to_le_bytes());
        eat(&[s.tree, s.mode]);
        eat(&s.bits.to_le_bytes());
    }
    h
}
