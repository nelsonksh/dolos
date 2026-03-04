use futures_core::Stream;
use futures_util::StreamExt;
use pallas::interop::utxorpc::spec as u5c;
use pallas::interop::utxorpc::{self as interop, LedgerContext};
use pallas::{
    interop::utxorpc::spec::watch::any_chain_tx_pattern::Chain,
    ledger::{
        addresses::Address,
        traverse::{MultiEraBlock, MultiEraOutput},
    },
};
use std::{collections::HashMap, pin::Pin, sync::Arc};
use tonic::{Request, Response, Status};

use super::stream::ChainStream;
use crate::prelude::*;

fn outputs_match_address(
    pattern: &u5c::cardano::AddressPattern,
    outputs: &[u5c::cardano::TxOutput],
) -> bool {
    let exact_matches = pattern.exact_address.is_empty()
        || outputs.iter().any(|o| o.address == pattern.exact_address);

    let delegation_matches = pattern.delegation_part.is_empty()
        || outputs.iter().any(|o| {
            let addr = Address::from_bytes(&o.address).unwrap();
            match addr {
                Address::Shelley(s) => s.delegation().to_vec().eq(&pattern.delegation_part),
                _ => false,
            }
        });

    let payment_matches = pattern.payment_part.is_empty()
        || outputs.iter().any(|o| {
            let addr = Address::from_bytes(&o.address).unwrap();
            match addr {
                Address::Shelley(s) => s.payment().to_vec().eq(&pattern.payment_part),
                _ => false,
            }
        });

    exact_matches && delegation_matches && payment_matches
}

fn outputs_match_asset(
    asset_pattern: &u5c::cardano::AssetPattern,
    outputs: &[u5c::cardano::TxOutput],
) -> bool {
    outputs
        .iter()
        .any(|o| matches_asset(asset_pattern, &o.assets))
}

fn matches_asset(
    asset_pattern: &u5c::cardano::AssetPattern,
    assets: &[u5c::cardano::Multiasset],
) -> bool {
    assets.iter().any(|ma| {
        if !asset_pattern.policy_id.is_empty() && asset_pattern.policy_id.ne(&ma.policy_id) {
            return false;
        }
        if asset_pattern.asset_name.is_empty() {
            return true;
        }
        ma.assets
            .iter()
            .any(|ma| asset_pattern.asset_name.eq(&ma.name))
    })
}

fn matches_output(
    pattern: &u5c::cardano::TxOutputPattern,
    outputs: &[u5c::cardano::TxOutput],
) -> bool {
    let address_match = pattern
        .address
        .as_ref()
        .is_none_or(|addr_pattern| outputs_match_address(addr_pattern, outputs));

    let asset_match = pattern
        .asset
        .as_ref()
        .is_none_or(|asset_pattern| outputs_match_asset(asset_pattern, outputs));

    address_match && asset_match
}

fn matches_cardano_pattern(tx_pattern: &u5c::cardano::TxPattern, tx: &u5c::cardano::Tx) -> bool {
    let has_address_match = tx_pattern.has_address.as_ref().is_none_or(|addr_pattern| {
        let outputs: Vec<_> = tx.outputs.to_vec();
        let inputs: Vec<_> = tx
            .inputs
            .iter()
            .filter_map(|x| x.as_output.as_ref().cloned())
            .collect();

        outputs_match_address(addr_pattern, &inputs)
            || outputs_match_address(addr_pattern, &outputs)
    });

    let consumes_match = tx_pattern.consumes.as_ref().is_none_or(|out_pattern| {
        let inputs: Vec<_> = tx
            .inputs
            .iter()
            .filter_map(|x| x.as_output.as_ref().cloned())
            .collect();
        matches_output(out_pattern, &inputs)
    });

    let mints_asset_match = tx_pattern
        .mints_asset
        .as_ref()
        .is_none_or(|asset_pattern| matches_asset(asset_pattern, &tx.mint));

    let moves_asset_match = tx_pattern.moves_asset.as_ref().is_none_or(|asset_pattern| {
        let inputs: Vec<_> = tx
            .inputs
            .iter()
            .filter_map(|x| x.as_output.as_ref().cloned())
            .collect();
        outputs_match_asset(asset_pattern, &inputs)
            || outputs_match_asset(asset_pattern, &tx.outputs)
    });

    let produces_match = tx_pattern
        .produces
        .as_ref()
        .is_none_or(|out_pattern| matches_output(out_pattern, &tx.outputs));

    has_address_match && consumes_match && mints_asset_match && moves_asset_match && produces_match
}

fn matches_chain(chain: &Chain, tx: &u5c::cardano::Tx) -> bool {
    match chain {
        Chain::Cardano(tx_pattern) => matches_cardano_pattern(tx_pattern, tx),
    }
}

fn apply_predicate(predicate: &u5c::watch::TxPredicate, tx: &u5c::cardano::Tx) -> bool {
    let tx_matches = predicate
        .r#match
        .as_ref()
        .and_then(|pattern| pattern.chain.as_ref())
        .is_none_or(|chain| matches_chain(chain, tx));

    let not_clause = predicate.not.iter().any(|p| apply_predicate(p, tx));
    let and_clause = predicate.all_of.iter().all(|p| apply_predicate(p, tx));
    let or_clause =
        predicate.any_of.is_empty() || predicate.any_of.iter().any(|p| apply_predicate(p, tx));

    tx_matches && !not_clause && and_clause && or_clause
}

// Hydrates each input's `as_output` field using the pre-resolved inputs map
// stored in the WAL LogValue for this block. This is a direct HashMap lookup —
// no archive fetch, no block decode, no index query required.
//
// The WAL stores the full EraCbor for every UTxO consumed by a block at the
// time it was applied, keyed by TxoRef(tx_hash, output_index). This is exactly
// the data we need and is already available without any additional I/O.
fn fill_input_as_output<D: Domain + LedgerContext>(
    tx: &mut u5c::cardano::Tx,
    mapper: &interop::Mapper<D>,
    wal_inputs: &HashMap<TxoRef, Arc<EraCbor>>,
) {
    for input in tx.inputs.iter_mut() {
        let hash: [u8; 32] = match input.tx_hash.as_ref().try_into() {
            Ok(x) => x,
            Err(_) => continue,
        };

        let txo_ref = TxoRef(TxHash::from(hash), input.output_index as TxoIdx);

        if let Some(era_cbor) = wal_inputs.get(&txo_ref) {
            if let Ok(output) = MultiEraOutput::try_from(era_cbor.as_ref()) {
                input.as_output = Some(mapper.map_tx_output(&output, None));
            }
        }
    }
}

fn block_to_txs<C: LedgerContext + Domain>(
    block: &RawBlock,
    mapper: &interop::Mapper<C>,
    request: &u5c::watch::WatchTxRequest,
    // Pre-resolved inputs from the WAL LogValue for this block.
    wal_inputs: &HashMap<TxoRef, Arc<EraCbor>>,
) -> Vec<u5c::watch::AnyChainTx> {
    // RawBlock is Arc<BlockBody> = Arc<Vec<u8>>; deref to get the byte slice.
    let body: &[u8] = block.as_ref();

    let decoded = MultiEraBlock::decode(body).unwrap();
    let txs = decoded.txs();

    txs.iter()
        .map(|x: &pallas::ledger::traverse::MultiEraTx<'_>| mapper.map_tx(x))
        .filter(|tx| {
            request
                .predicate
                .as_ref()
                .is_none_or(|predicate| apply_predicate(predicate, tx))
        })
        .map(|mut tx| {
            fill_input_as_output(&mut tx, mapper, wal_inputs);
            tx
        })
        .map(|x| u5c::watch::AnyChainTx {
            chain: Some(u5c::watch::any_chain_tx::Chain::Cardano(x)),
            block: Some(u5c::watch::AnyChainBlock {
                native_bytes: body.to_vec().into(),
                chain: Some(u5c::watch::any_chain_block::Chain::Cardano(
                    mapper.map_block_cbor(body),
                )),
            }),
        })
        .collect()
}

// Fetches the pre-resolved inputs map from the WAL for a given ChainPoint.
// Returns an empty map if the entry is missing or the WAL read fails, so
// callers get `as_output: None` for all inputs rather than a hard error.
fn wal_inputs_for<D: Domain>(
    domain: &D,
    point: &ChainPoint,
) -> HashMap<TxoRef, Arc<EraCbor>> {
    domain
        .wal()
        .read_entry(point)
        .ok()
        .flatten()
        .map(|log| log.inputs)
        .unwrap_or_default()
}

// ChainStream yields TipEvent:
//   Apply(ChainPoint, RawBlock) — roll forward
//   Undo(ChainPoint, RawBlock)  — rollback
//   Mark(ChainPoint)            — intersect / origin marker
//
// For Apply and Undo we fetch the WAL LogValue by ChainPoint to get the
// pre-resolved inputs map, then pass it down to block_to_txs.
fn roll_to_watch_response<C: LedgerContext + Domain>(
    mapper: &interop::Mapper<C>,
    log: &TipEvent,
    request: &u5c::watch::WatchTxRequest,
    domain: &C,
) -> impl Stream<Item = u5c::watch::WatchTxResponse> {
    let txs: Vec<_> = match log {
        TipEvent::Apply(point, block) => {
            let wal_inputs = wal_inputs_for(domain, point);
            block_to_txs(block, mapper, request, &wal_inputs)
                .into_iter()
                .map(u5c::watch::watch_tx_response::Action::Apply)
                .map(|x| u5c::watch::WatchTxResponse { action: Some(x) })
                .collect()
        }
        TipEvent::Undo(point, block) => {
            let wal_inputs = wal_inputs_for(domain, point);
            block_to_txs(block, mapper, request, &wal_inputs)
                .into_iter()
                .map(u5c::watch::watch_tx_response::Action::Undo)
                .map(|x| u5c::watch::WatchTxResponse { action: Some(x) })
                .collect()
        }
        TipEvent::Mark(..) => vec![],
    };

    tokio_stream::iter(txs)
}

pub struct WatchServiceImpl<D, C>
where
    D: Domain + LedgerContext,
    C: CancelToken,
{
    domain: D,
    mapper: interop::Mapper<D>,
    cancel: C,
}

impl<D, C> WatchServiceImpl<D, C>
where
    D: Domain + LedgerContext,
    C: CancelToken,
{
    pub fn new(domain: D, cancel: C) -> Self {
        let mapper = interop::Mapper::new(domain.clone());

        Self {
            domain,
            mapper,
            cancel,
        }
    }
}

#[async_trait::async_trait]
impl<D, C> u5c::watch::watch_service_server::WatchService for WatchServiceImpl<D, C>
where
    D: Domain + LedgerContext,
    C: CancelToken,
{
    type WatchTxStream = Pin<
        Box<dyn Stream<Item = Result<u5c::watch::WatchTxResponse, tonic::Status>> + Send + 'static>,
    >;

    async fn watch_tx(
        &self,
        request: Request<u5c::watch::WatchTxRequest>,
    ) -> Result<Response<Self::WatchTxStream>, Status> {
        let inner_req = request.into_inner();

        let intersect = inner_req
            .intersect
            .iter()
            .map(|x| ChainPoint::Specific(x.slot, x.hash.to_vec().as_slice().into()))
            .collect::<Vec<ChainPoint>>();

        // ChainStream::start now takes (domain, intersect, cancel) — 3 args.
        let stream = ChainStream::start::<D, C>(
            self.domain.clone(),
            intersect,
            self.cancel.clone(),
        );

        let mapper = self.mapper.clone();
        let domain = self.domain.clone();

        let stream = stream
            .flat_map(move |log| roll_to_watch_response(&mapper, &log, &inner_req, &domain))
            .map(Ok);

        Ok(Response::new(Box::pin(stream)))
    }
}