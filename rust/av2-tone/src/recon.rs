//! Reconstruction, stage by stage. So far: every transform block's
//! dequantized coefficients, as AVM hands them to its inverse transform
//! (av2_inverse_transform_block), checked block by block against AVM by
//! `av2-check --dequant`; and each block's residual ([`crate::itx::residual`]),
//! checked by `av2-check --recon` as AVM's prediction plus the residual
//! against AVM's reconstruction.

/// One transform block, as the inverse transform will receive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxBlock {
    pub plane: u8,
    /// The block's top-left in its plane, in samples.
    pub x: u32,
    pub y: u32,
    /// A TX_SIZE and a TX_TYPE (secondary-transform bits included).
    pub tx_size: u8,
    pub tx_type: u16,
    /// The eob AVM passes on: for chroma under CCTX, the larger of U's and V's.
    pub eob: u16,
    /// Dequantized, after CCTX, at each coefficient's position in the
    /// adjusted transform size (64-point sides keep their first 32).
    pub coeffs: Vec<i32>,
    /// get_intra_mode for the block's plane: its prediction mode (a chroma
    /// mode mapped to luma's), or the wide-angle mode it maps to for this
    /// transform's shape. The secondary transform reads it.
    pub mode: u8,
    /// is_inter_block, which on a key frame means intra block copy.
    pub inter: bool,
}

/// The transform blocks of the key frame in an OBU stream, in decode order.
pub fn transform_blocks(stream: &[u8]) -> crate::Result<(crate::Headers, Vec<TxBlock>)> {
    let (headers, group) = crate::parse_obus(stream)?;
    let blocks = crate::parse::parse_tiles(&headers, group.data, None)?;
    Ok((headers, blocks))
}
