use std::collections::HashMap;

use tokio::task;
use tokio_stream::{self as stream, StreamExt};

use alloy::{
    network::Network,
    primitives::{Address, B256},
    providers::Provider,
    rpc::types::eth::Filter,
    sol_types::SolEvent,
    transports::Transport,
};

use crate::{
    amm::{
        balancer_v2::factory::IBFactory, factory::Factory, uniswap_v2::factory::IUniswapV2Factory,
        uniswap_v3::factory::IUniswapV3Factory,
    },
    errors::AMMError,
};

pub enum DiscoverableFactory {
    UniswapV2Factory,
    UniswapV3Factory,
    BalancerV2Factory,
}

impl DiscoverableFactory {
    pub fn discovery_event_signature(&self) -> B256 {
        match self {
            DiscoverableFactory::UniswapV2Factory => IUniswapV2Factory::PairCreated::SIGNATURE_HASH,
            DiscoverableFactory::UniswapV3Factory => IUniswapV3Factory::PoolCreated::SIGNATURE_HASH,
            DiscoverableFactory::BalancerV2Factory => IBFactory::LOG_NEW_POOL::SIGNATURE_HASH,
        }
    }
}

// Returns a vec of empty factories that match one of the Factory interfaces specified by each DiscoverableFactory
pub async fn discover_factories<T, N, P>(
    factories: Vec<DiscoverableFactory>,
    number_of_amms_threshold: u64,
    provider: P,
    step: u64,
) -> Result<Vec<Factory>, AMMError>
where
    T: Transport + Clone,
    N: Network,
    P: Provider<T, N> + Clone,
{
    let mut event_signatures = vec![];

    for factory in factories {
        event_signatures.push(factory.discovery_event_signature());
    }
    tracing::trace!(?event_signatures);

    let block_filter = Filter::new().event_signature(event_signatures);

    let mut from_block = 0;
    let current_block = provider.get_block_number().await?;

    // For each block within the range, get all pairs asynchronously
    // let step = 100000;

    // Set up filter and events to filter each block you are searching by
    let mut identified_factories: HashMap<Address, (Factory, u64)> = HashMap::new();

    // set up a vector with the block range for each batch
    let mut block_num_vec: Vec<(u64, u64)> = Vec::new();

    // populate the vector
    while from_block < block_number {
        // Get pair created event logs within the block range
        let mut target_block = from_block + block_step - 1;
        if target_block > block_number {
            target_block = block_number;
        }

        block_num_vec.push((from_block, target_block));

        from_block += block_step;
    }

    // Create stream to process block async
    let stream = stream::iter(&block_num_vec).map(|&(from_block, target_block)| {
        let block_filter = block_filter.clone();
        let client = client.clone();
        task::spawn(async move {
            process_block_logs_batch(&from_block, &target_block, client, &block_filter).await
        })
    });

    // collect the results of the stream in a vector
    let results = stream.collect::<Vec<_>>().await;

    for result in results {
        match result.await {
            Ok(Ok(local_identified_factories)) => {
                for (addrs, count) in local_identified_factories {
                    *identified_factories.entry(addrs).or_insert(0) += count;
                }
            }
            Ok(Err(err)) => {
                // The task ran successfully, but there was an error in the Result.
                eprintln!("Error occurred: {:?}", err);
            }
            Err(join_err) => {
                // The task itself failed (possibly panicked).
                eprintln!("Task join error: {:?}", join_err);
            }
        }
    }

    let mut filtered_factories = vec![];
    tracing::trace!(number_of_amms_threshold, "checking threshold");
    for (address, (factory, amms_length)) in identified_factories {
        if amms_length >= number_of_amms_threshold {
            tracing::trace!("factory {} has {} AMMs => adding", address, amms_length);
            filtered_factories.push(factory);
        } else {
            tracing::trace!("factory {} has {} AMMs => skipping", address, amms_length);
        }
    }

    Ok(filtered_factories)
}

async fn process_block_logs_batch(
    from_block: &u64,
    target_block: &u64,
    client: RootProvider<Http<Client>>,
    block_filter: &Filter,
) -> anyhow::Result<HashMap<Address, u64>> {
    let block_filter = block_filter.clone();
    let mut local_identified_factories: HashMap<Address, u64> = HashMap::new();

    let logs = client
        .get_logs(&block_filter.from_block(*from_block).to_block(*target_block))
        .await?;

    for log in logs {
        if let Some((_, amms_length)) = local_identified_factories.get_mut(&log.address()) {
            *amms_length += 1;
        } else {
            let mut factory = Factory::try_from(log.topics()[0])?;

            match &mut factory {
                Factory::UniswapV2Factory(uniswap_v2_factory) => {
                    uniswap_v2_factory.address = log.address();
                    uniswap_v2_factory.creation_block =
                        log.block_number.ok_or(AMMError::BlockNumberNotFound)?;
                }
                Factory::UniswapV3Factory(uniswap_v3_factory) => {
                    uniswap_v3_factory.address = log.address();
                    uniswap_v3_factory.creation_block =
                        log.block_number.ok_or(AMMError::BlockNumberNotFound)?;
                }
                Factory::BalancerV2Factory(balancer_v2_factory) => {
                    balancer_v2_factory.address = log.address();
                    balancer_v2_factory.creation_block =
                        log.block_number.ok_or(AMMError::BlockNumberNotFound)?;
                }
            }

            local_identified_factories.insert(log.address(), (factory, 0));
        }
    }

    Ok(local_identified_factories)
}
