//! Parsed headers as `key value...` lines, one field per line. The oracle
//! (../../oracle/av2oracle.c) prints the same keys from AVM's own structs, so
//! a line diff between the two is a field-by-field check against AVM.
//!
//! A field AVM doesn't set on a path is left out on that path, here and in
//! the oracle alike, rather than compared against whatever AVM left there.

use std::fmt::Write;

use crate::Headers;

fn b(v: bool) -> u32 {
    u32::from(v)
}

fn list<T: std::fmt::Display>(v: impl IntoIterator<Item = T>) -> String {
    v.into_iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ")
}

pub fn dump(h: &Headers) -> String {
    let mut o = String::new();
    let mut line = |k: &str, v: String| {
        let _ = writeln!(o, "{k} {v}");
    };
    let s = &h.seq;
    line("seq.id", s.seq_header_id.to_string());
    line("seq.profile", s.profile.to_string());
    line("seq.single_picture", b(s.single_picture_header_flag).to_string());
    line("seq.level_tier", list([s.level, s.tier]));
    line("seq.format", list([s.bit_depth, s.subsampling_x, s.subsampling_y, b(s.monochrome)]));
    line("seq.still", b(s.still_picture).to_string());
    line("seq.max_size", list([s.max_frame_width, s.max_frame_height, s.num_bits_width, s.num_bits_height]));
    line(
        "seq.conf_win",
        list([b(s.conf_win_enabled), s.conf_win_left, s.conf_win_right, s.conf_win_top, s.conf_win_bottom]),
    );
    line("seq.sb_size", s.sb_size.to_string());
    line(
        "seq.partition",
        list([
            b(s.enable_sdp),
            b(s.enable_extended_sdp),
            b(s.enable_ext_partitions),
            b(s.enable_uneven_4way_partitions),
            s.max_pb_aspect_ratio_log2_m1,
        ]),
    );
    line("seq.segment", list([b(s.enable_ext_seg), b(s.seq_seg_info_present_flag)]));
    if s.seq_seg_info_present_flag {
        let g = &s.seg;
        line("seq.seg_change", b(g.allow_seg_info_change).to_string());
        line("seq.seg_mask", list(g.feature_mask));
        line("seq.seg_alt_q", list(g.feature_data.iter().map(|d| d[0])));
        line("seq.seg_derived", list([b(g.segid_preskip), g.last_active_segid]));
    }
    line(
        "seq.intra",
        list([
            b(s.enable_intra_dip),
            b(s.enable_intra_edge_filter),
            b(s.enable_mrls),
            b(s.enable_cfl_intra),
            s.cfl_ds_filter_index,
            b(s.enable_mhccp),
            b(s.enable_ibp),
        ]),
    );
    line(
        "seq.inter",
        list([
            b(s.enable_refmvbank),
            s.enable_drl_reorder,
            s.ref_frames,
            s.def_max_bvp_drl_bits,
            b(s.allow_frame_max_bvp_drl_bits),
            b(s.enable_bawp),
        ]),
    );
    line("seq.scc", list([s.force_screen_content_tools, s.force_integer_mv]));
    line(
        "seq.transform",
        list([
            b(s.enable_fsc),
            b(s.enable_idtx_intra),
            b(s.enable_ist),
            b(s.enable_inter_ist),
            b(s.enable_chroma_dctonly),
            b(s.reduced_tx_part_set),
            b(s.enable_cctx),
        ]),
    );
    line(
        "seq.quant",
        list([s.enable_tcq, b(s.enable_parity_hiding), b(s.enable_avg_cdf), s.avg_cdf_type, b(s.separate_uv_delta_q)]),
    );
    line(
        "seq.dc_ac",
        list([
            i32::from(s.equal_ac_dc_q),
            s.base_y_dc_delta_q,
            i32::from(s.y_dc_delta_q_enabled),
            s.base_uv_dc_delta_q,
            i32::from(s.uv_dc_delta_q_enabled),
            s.base_uv_ac_delta_q,
            i32::from(s.uv_ac_delta_q_enabled),
        ]),
    );
    line(
        "seq.filter",
        list([
            b(s.disable_loopfilters_across_tiles),
            b(s.enable_cdef),
            b(s.enable_gdf),
            b(s.gdf_unit_matches_sb_size),
            b(s.enable_ccso),
            b(s.ccso_unit_matches_sb_size),
            s.enable_cdef_on_skip_txfm,
            s.df_par_bits_minus2,
        ]),
    );
    line("seq.restoration", b(s.enable_restoration).to_string());
    if s.enable_restoration {
        line("seq.lr_disable", list(s.lr_tools_disable_mask));
    }
    line("seq.tile_info", b(s.seq_tile_info_present_flag).to_string());
    line("seq.film_grain", b(s.film_grain_params_present).to_string());

    let f = &h.frame;
    let planes = s.num_planes();
    let segs = if s.enable_ext_seg { 16 } else { 8 };
    line("frame.ids", list([f.cur_mfh_id, f.seq_header_id]));
    line("frame.type_show_refresh", list([0, 1, f.refresh_frame_flags])); // KEY_FRAME, shown
    line("frame.sb_size", f.sb_size.to_string());
    line("frame.size", list([f.width, f.height, f.mi_cols, f.mi_rows]));
    line("frame.scc", list([b(f.allow_screen_content_tools), b(f.cur_frame_force_integer_mv)]));
    line("frame.intrabc", list([b(f.allow_intrabc), b(f.allow_global_intrabc), b(f.allow_local_intrabc)]));
    if f.allow_intrabc {
        line("frame.bvp_drl", f.max_bvp_drl_bits.to_string());
    }
    line("frame.cdf_off", b(f.disable_cdf_update).to_string());
    let t = &f.tiles;
    line("frame.tiles", list([b(t.uniform), t.cols, t.rows, t.log2_cols, t.log2_rows]));
    line("frame.tile_cols", list(&t.col_start_sb));
    line("frame.tile_rows", list(&t.row_start_sb));
    if t.cols * t.rows > 1 {
        line("frame.tile_bytes", list([t.context_update_tile_id, t.tile_size_bytes]));
    }
    let q = &f.quant;
    line(
        "frame.q",
        list([q.base_qindex as i32, q.y_dc_delta_q, q.u_dc_delta_q, q.u_ac_delta_q, q.v_dc_delta_q, q.v_ac_delta_q]),
    );
    let g = &f.seg;
    line("frame.seg", b(g.enabled).to_string());
    if g.enabled {
        line(
            "frame.seg_flags",
            list([b(g.update_map), b(g.update_data), b(g.temporal_update), b(g.segid_preskip), g.last_active_segid]),
        );
        line("frame.seg_mask", list(g.feature_mask.iter().take(segs)));
        line("frame.seg_alt_q", list(g.feature_data.iter().take(segs).map(|d| d[0])));
    }
    line("frame.qm", b(q.using_qmatrix).to_string());
    if q.using_qmatrix {
        let n = q.pic_qm_num as usize;
        line("frame.qm_y", list(q.qm_y.iter().take(n)));
        if planes > 1 {
            line("frame.qm_u", list(q.qm_u.iter().take(n)));
            line("frame.qm_v", list(q.qm_v.iter().take(n)));
        }
        let idx = q.lossless.iter().zip(q.qm_index).take(segs).filter(|(l, _)| !**l).map(|(_, i)| i);
        line("frame.qm_index", list(idx));
    }
    line("frame.delta_q", list([b(q.delta_q_present), q.delta_q_res]));
    line("frame.qindex", list(q.qindex.iter().take(segs)));
    line("frame.lossless", list(q.lossless.iter().take(segs).map(|&l| b(l))));
    line("frame.coded_lossless", b(f.coded_lossless).to_string());
    line("frame.tcq_ph", list([f.tcq_mode, b(f.allow_parity_hiding)]));
    let d = &f.deblock;
    line(
        "frame.deblock",
        list([
            i32::from(d.apply[0]),
            i32::from(d.apply[1]),
            i32::from(d.apply_u),
            i32::from(d.apply_v),
            d.delta_q_luma[0],
            d.delta_q_luma[1],
            d.delta_q_u,
            d.delta_q_v,
        ]),
    );
    if s.enable_gdf {
        let g = &f.gdf;
        line("frame.gdf", list([g.mode, g.pic_qp_idx, g.pic_scale_idx, g.block_size, g.block_num]));
    }
    if s.enable_cdef {
        let c = &f.cdef;
        line("frame.cdef", list([b(c.enable), c.damping, c.strengths.len() as u32, b(c.on_skip_txfm)]));
        line("frame.cdef_y", list(&c.strengths));
        line("frame.cdef_uv", list(&c.uv_strengths));
    }
    if s.enable_restoration {
        line("frame.lr_type", list((0..planes).map(|_| 0)));
        line("frame.lr_unit", list(f.restoration_unit_size.iter().take(planes)));
    }
    if s.enable_ccso {
        let c = &f.ccso;
        line("frame.ccso", list([b(c.frame_flag), c.blk_size_log2]));
        for (i, p) in c.planes.iter().enumerate() {
            line(&format!("frame.ccso{i}"), b(p.enable).to_string());
            if p.enable {
                line(
                    &format!("frame.ccso{i}_params"),
                    list([b(p.bo_only), p.scale_idx, p.quant_idx, p.ext_filter_support, p.edge_clf, p.max_band_log2]),
                );
                line(&format!("frame.ccso{i}_offsets"), list(&p.offsets));
            }
        }
    }
    line("frame.tx", list([f.tx_mode, f.reduced_tx_set_used]));
    line("frame.header_bits", f.header_bits.to_string());
    o
}
