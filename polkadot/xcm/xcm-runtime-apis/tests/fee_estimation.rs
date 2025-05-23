// Copyright (C) Parity Technologies (UK) Ltd.
// This file is part of Polkadot.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Tests for using both the XCM fee payment API and the dry-run API.

use frame_support::sp_runtime::testing::H256;
use frame_system::RawOrigin;
use sp_api::ProvideRuntimeApi;
use xcm::prelude::*;
use xcm_runtime_apis::{
	dry_run::{CallDryRunEffects, DryRunApi},
	fees::XcmPaymentApi,
};

mod mock;
use mock::{
	new_test_ext_with_balances, new_test_ext_with_balances_and_assets, DeliveryFees,
	ExistentialDeposit, HereLocation, OriginCaller, RuntimeCall, RuntimeEvent, TestClient,
};
use xcm_simulator::fake_message_hash;

// Scenario: User `1` in the local chain (id 2000) wants to transfer assets to account `[0u8; 32]`
// on "AssetHub". He wants to make sure he has enough for fees, so before he calls the
// `transfer_asset` extrinsic to do the transfer, he decides to use the `XcmDryRunApi` and
// `XcmPaymentApi` runtime APIs to estimate fees. This uses a teleport because we're dealing with
// the native token of the chain, which is registered on "AssetHub". The fees are sent as a reserve
// asset transfer, since they're paid in the relay token.
//
//                 Teleport Parachain(2000) Token
//                 Reserve Asset Transfer Relay Token for fees
// Parachain(2000) -------------------------------------------> Parachain(1000)
#[test]
fn fee_estimation_for_teleport() {
	sp_tracing::init_for_tests();
	let who = 1; // AccountId = u64.
	let balances = vec![(who, 100 + DeliveryFees::get() + ExistentialDeposit::get())];
	let assets = vec![(1, who, 50)];
	new_test_ext_with_balances_and_assets(balances, assets).execute_with(|| {
		let client = TestClient;
		let runtime_api = client.runtime_api();
		let call = RuntimeCall::XcmPallet(pallet_xcm::Call::transfer_assets {
			dest: Box::new(VersionedLocation::from((Parent, Parachain(1000)))),
			beneficiary: Box::new(VersionedLocation::from(AccountId32 {
				id: [0u8; 32],
				network: None,
			})),
			assets: Box::new(VersionedAssets::from(vec![
				(Here, 100u128).into(),
				(Parent, 20u128).into(),
			])),
			fee_asset_item: 1, // Fees are paid with the RelayToken
			weight_limit: Unlimited,
		});
		let origin = OriginCaller::system(RawOrigin::Signed(who));
		let dry_run_effects = runtime_api
			.dry_run_call(H256::zero(), origin, call, XCM_VERSION)
			.unwrap()
			.unwrap();

		assert_eq!(
			dry_run_effects.local_xcm,
			Some(VersionedXcm::from(
				Xcm::builder_unsafe()
					.withdraw_asset((Parent, 20u128))
					.burn_asset((Parent, 20u128))
					.withdraw_asset((Here, 100u128))
					.burn_asset((Here, 100u128))
					.build()
			)),
		);
		let send_destination = Location::new(1, [Parachain(1000)]);
		let send_message = Xcm::<()>::builder_unsafe()
			.withdraw_asset((Parent, 20u128))
			.buy_execution((Parent, 20u128), Unlimited)
			.receive_teleported_asset(((Parent, Parachain(2000)), 100u128))
			.clear_origin()
			.deposit_asset(AllCounted(2), [0u8; 32])
			.build();
		assert_eq!(
			dry_run_effects.forwarded_xcms,
			vec![(
				VersionedLocation::from(send_destination.clone()),
				vec![VersionedXcm::from(send_message.clone())],
			),],
		);

		assert_eq!(
			dry_run_effects.emitted_events,
			vec![
				RuntimeEvent::System(frame_system::Event::NewAccount {
					account: 8660274132218572653 // TODO: Why is this not `1`?
				}),
				RuntimeEvent::Balances(pallet_balances::Event::Endowed {
					account: 8660274132218572653,
					free_balance: 100
				}),
				RuntimeEvent::Balances(pallet_balances::Event::Minted {
					who: 8660274132218572653,
					amount: 100
				}),
				RuntimeEvent::AssetsPallet(pallet_assets::Event::Burned {
					asset_id: 1,
					owner: 1,
					balance: 20
				}),
				RuntimeEvent::Balances(pallet_balances::Event::Burned { who: 1, amount: 100 }),
				RuntimeEvent::XcmPallet(pallet_xcm::Event::Attempted {
					outcome: Outcome::Complete { used: Weight::from_parts(400, 40) },
				}),
				RuntimeEvent::Balances(pallet_balances::Event::Burned { who: 1, amount: 20 }),
				RuntimeEvent::XcmPallet(pallet_xcm::Event::FeesPaid {
					paying: AccountIndex64 { index: 1, network: None }.into(),
					fees: (Here, 20u128).into(),
				}),
				RuntimeEvent::XcmPallet(pallet_xcm::Event::Sent {
					origin: AccountIndex64 { index: 1, network: None }.into(),
					destination: (Parent, Parachain(1000)).into(),
					message: send_message.clone(),
					message_id: fake_message_hash(&send_message),
				}),
			]
		);

		// Weighing the local program is not relevant for extrinsics that already
		// take this weight into account.
		// In this case, we really only care about delivery fees.
		let local_xcm = dry_run_effects.local_xcm.unwrap();

		// We get a double result since the actual call returns a result and the runtime api returns
		// results.
		let weight =
			runtime_api.query_xcm_weight(H256::zero(), local_xcm.clone()).unwrap().unwrap();
		assert_eq!(weight, Weight::from_parts(400, 40));
		let execution_fees = runtime_api
			.query_weight_to_asset_fee(
				H256::zero(),
				weight,
				VersionedAssetId::from(AssetId(HereLocation::get())),
			)
			.unwrap()
			.unwrap();
		assert_eq!(execution_fees, 440);

		let mut forwarded_xcms_iter = dry_run_effects.forwarded_xcms.into_iter();

		let (destination, remote_messages) = forwarded_xcms_iter.next().unwrap();
		let remote_message = &remote_messages[0];

		let delivery_fees = runtime_api
			.query_delivery_fees(H256::zero(), destination.clone(), remote_message.clone())
			.unwrap()
			.unwrap();
		assert_eq!(delivery_fees, VersionedAssets::from((Here, 20u128)));

		// This would have to be the runtime API of the destination,
		// which we have the location for.
		// If I had a mock runtime configured for "AssetHub" then I would use the
		// runtime APIs from that.
		let remote_execution_weight = runtime_api
			.query_xcm_weight(H256::zero(), remote_message.clone())
			.unwrap()
			.unwrap();
		let remote_execution_fees = runtime_api
			.query_weight_to_asset_fee(
				H256::zero(),
				remote_execution_weight,
				VersionedAssetId::from(AssetId(HereLocation::get())),
			)
			.unwrap()
			.unwrap();
		assert_eq!(remote_execution_fees, 550);

		// Now we know that locally we need to use `execution_fees` and
		// `delivery_fees`.
		// On the message we forward to the destination, we need to
		// put `remote_execution_fees` in `BuyExecution`.
		// For the `transfer_assets` extrinsic, it just means passing the correct amount
		// of fees in the parameters.
	});
}

// Same scenario as in `fee_estimation_for_teleport`, but the user in parachain 2000 wants
// to send relay tokens over to parachain 1000.
//
//                 Reserve Asset Transfer Relay Token
//                 Reserve Asset Transfer Relay Token for fees
// Parachain(2000) -------------------------------------------> Parachain(1000)
fn dry_run_reserve_asset_transfer_common(
	input_xcm_version: XcmVersion,
	expected_result_xcms_version: XcmVersion,
	dry_run_call: impl FnOnce(&TestClient, OriginCaller, RuntimeCall) -> CallDryRunEffects<RuntimeEvent>,
) {
	sp_tracing::init_for_tests();
	let who = 1; // AccountId = u64.
			  // Native token used for fees.
	let balances = vec![(who, DeliveryFees::get() + ExistentialDeposit::get())];
	// Relay token is the one we want to transfer.
	let assets = vec![(1, who, 100)]; // id, account_id, balance.
	new_test_ext_with_balances_and_assets(balances, assets).execute_with(|| {
		let call = RuntimeCall::XcmPallet(pallet_xcm::Call::transfer_assets {
			dest: Box::new(
				VersionedLocation::from((Parent, Parachain(1000)))
					.into_version(input_xcm_version)
					.unwrap(),
			),
			beneficiary: Box::new(
				VersionedLocation::from(AccountId32 { id: [0u8; 32], network: None })
					.into_version(input_xcm_version)
					.unwrap(),
			),
			assets: Box::new(
				VersionedAssets::from((Parent, 100u128))
					.into_version(input_xcm_version)
					.unwrap(),
			),
			fee_asset_item: 0,
			weight_limit: Unlimited,
		});
		let origin = OriginCaller::system(RawOrigin::Signed(who));
		let dry_run_effects = dry_run_call(&TestClient, origin, call);

		assert_eq!(
			dry_run_effects.local_xcm,
			Some(
				VersionedXcm::from(
					Xcm::builder_unsafe()
						.withdraw_asset((Parent, 100u128))
						.burn_asset((Parent, 100u128))
						.build()
				)
				.into_version(expected_result_xcms_version)
				.unwrap()
			),
		);

		// In this case, the transfer type is `DestinationReserve`, so the remote xcm just withdraws
		// the assets.
		let send_destination = Location::new(1, Parachain(1000));
		let send_message = Xcm::<()>::builder_unsafe()
			.withdraw_asset((Parent, 100u128))
			.clear_origin()
			.buy_execution((Parent, 100u128), Unlimited)
			.deposit_asset(AllCounted(1), [0u8; 32])
			.build();
		assert_eq!(
			dry_run_effects.forwarded_xcms,
			vec![(
				VersionedLocation::from(send_destination.clone())
					.into_version(expected_result_xcms_version)
					.unwrap(),
				vec![VersionedXcm::from(send_message.clone())
					.into_version(expected_result_xcms_version)
					.unwrap()],
			),],
		);

		assert_eq!(
			dry_run_effects.emitted_events,
			vec![
				RuntimeEvent::AssetsPallet(pallet_assets::Event::Burned {
					asset_id: 1,
					owner: 1,
					balance: 100
				}),
				RuntimeEvent::XcmPallet(pallet_xcm::Event::Attempted {
					outcome: Outcome::Complete { used: Weight::from_parts(200, 20) }
				}),
				RuntimeEvent::Balances(pallet_balances::Event::Burned { who: 1, amount: 20 }),
				RuntimeEvent::XcmPallet(pallet_xcm::Event::FeesPaid {
					paying: AccountIndex64 { index: 1, network: None }.into(),
					fees: (Here, 20u128).into()
				}),
				RuntimeEvent::XcmPallet(pallet_xcm::Event::Sent {
					origin: AccountIndex64 { index: 1, network: None }.into(),
					destination: send_destination.clone(),
					message: send_message.clone(),
					message_id: fake_message_hash(&send_message),
				}),
			]
		);
	});
}

#[test]
fn dry_run_reserve_asset_transfer_xcm_versions() {
	let tested_versions = MIN_XCM_VERSION..=XCM_VERSION;

	for version in tested_versions {
		let input_version = version;
		let expected_result_xcms_version = version;
		dry_run_reserve_asset_transfer_common(
			input_version,
			expected_result_xcms_version,
			|client, origin, call| {
				client
					.runtime_api()
					.dry_run_call(H256::zero(), origin, call, expected_result_xcms_version)
					.unwrap()
					.unwrap()
			},
		);
	}
}

#[test]
fn dry_run_before_api_v2_reserve_asset_transfer() {
	let tested_versions = MIN_XCM_VERSION..=XCM_VERSION;

	for version in tested_versions {
		let input_version = version;
		let expected_result_xcms_version = XCM_VERSION;
		dry_run_reserve_asset_transfer_common(
			input_version,
			expected_result_xcms_version,
			|client, origin, call| {
				#[allow(deprecated)]
				client
					.runtime_api()
					.dry_run_call_before_version_2(H256::zero(), origin, call)
					.unwrap()
					.unwrap()
			},
		);
	}
}

fn dry_run_xcm_common(xcm_version: XcmVersion) {
	sp_tracing::init_for_tests();
	let who = 1; // AccountId = u64.
	let transfer_amount = 100u128;
	// We need to build the XCM to weigh it and then build the real XCM that can pay for fees.
	let inner_xcm = Xcm::<()>::builder_unsafe()
		.buy_execution((Here, 1u128), Unlimited) // We'd need to query the destination chain for fees.
		.deposit_asset(AllCounted(1), [0u8; 32])
		.build();
	let xcm_to_weigh = Xcm::<RuntimeCall>::builder_unsafe()
		.withdraw_asset((Here, transfer_amount))
		.clear_origin()
		.buy_execution((Here, transfer_amount), Unlimited)
		.deposit_reserve_asset(AllCounted(1), (Parent, Parachain(2100)), inner_xcm.clone())
		.build();
	let client = TestClient;
	let runtime_api = client.runtime_api();
	let xcm_weight = runtime_api
		.query_xcm_weight(
			H256::zero(),
			VersionedXcm::from(xcm_to_weigh.clone().into())
				.into_version(xcm_version)
				.unwrap(),
		)
		.unwrap()
		.unwrap();
	let execution_fees = runtime_api
		.query_weight_to_asset_fee(
			H256::zero(),
			xcm_weight,
			VersionedAssetId::from(AssetId(Here.into())).into_version(xcm_version).unwrap(),
		)
		.unwrap()
		.unwrap();
	let xcm = Xcm::<RuntimeCall>::builder_unsafe()
		.withdraw_asset((Here, transfer_amount + execution_fees))
		.clear_origin()
		.buy_execution((Here, execution_fees), Unlimited)
		.deposit_reserve_asset(AllCounted(1), (Parent, Parachain(2100)), inner_xcm.clone())
		.build();
	let expected_msg_id = fake_message_hash(&xcm);
	let balances = vec![(
		who,
		transfer_amount + execution_fees + DeliveryFees::get() + ExistentialDeposit::get(),
	)];
	new_test_ext_with_balances(balances).execute_with(|| {
		let dry_run_effects = runtime_api
			.dry_run_xcm(
				H256::zero(),
				VersionedLocation::from([AccountIndex64 { index: 1, network: None }])
					.into_version(xcm_version)
					.unwrap(),
				VersionedXcm::from(xcm).into_version(xcm_version).unwrap(),
			)
			.unwrap()
			.unwrap();
		let expected_xcms = Xcm::<()>::builder_unsafe()
			.reserve_asset_deposited((
				(Parent, Parachain(2000)),
				transfer_amount + execution_fees - DeliveryFees::get(),
			))
			.clear_origin()
			.buy_execution((Here, 1u128), Unlimited)
			.deposit_asset(AllCounted(1), [0u8; 32])
			.set_topic(expected_msg_id)
			.build();
		assert_eq!(
			dry_run_effects.forwarded_xcms,
			vec![(
				VersionedLocation::from((Parent, Parachain(2100)))
					.into_version(xcm_version)
					.unwrap(),
				vec![VersionedXcm::from(expected_xcms).into_version(xcm_version).unwrap()],
			),]
		);

		assert_eq!(
			dry_run_effects.emitted_events,
			vec![
				RuntimeEvent::Balances(pallet_balances::Event::Burned { who: 1, amount: 540 }),
				RuntimeEvent::System(frame_system::Event::NewAccount { account: 2100 }),
				RuntimeEvent::Balances(pallet_balances::Event::Endowed {
					account: 2100,
					free_balance: 520
				}),
				RuntimeEvent::Balances(pallet_balances::Event::Minted { who: 2100, amount: 520 }),
				RuntimeEvent::XcmPallet(pallet_xcm::Event::Sent {
					origin: (who,).into(),
					destination: (Parent, Parachain(2100)).into(),
					message: Xcm::default(),
					message_id: expected_msg_id,
				})
			]
		);
	});
}

#[test]
fn dry_run_xcm_versions() {
	let tested_versions = [XCM_VERSION, 5, 4, 3];

	for version in tested_versions {
		dry_run_xcm_common(version);
	}
}

#[test]
fn calling_payment_api_with_a_lower_version_works() {
	let transfer_amount = 100u128;
	let xcm_to_weigh = Xcm::<RuntimeCall>::builder_unsafe()
		.withdraw_asset((Here, transfer_amount))
		.buy_execution((Here, transfer_amount), Unlimited)
		.deposit_asset(AllCounted(1), [1u8; 32])
		.build();
	let versioned_xcm_to_weigh = VersionedXcm::from(xcm_to_weigh.clone().into());
	let lower_version_xcm_to_weigh = versioned_xcm_to_weigh.into_version(XCM_VERSION - 1).unwrap();
	let client = TestClient;
	let runtime_api = client.runtime_api();
	let xcm_weight =
		runtime_api.query_xcm_weight(H256::zero(), lower_version_xcm_to_weigh).unwrap();
	assert!(xcm_weight.is_ok());
	let native_token = VersionedAssetId::from(AssetId(Here.into()));
	let lower_version_native_token = native_token.into_version(XCM_VERSION - 1).unwrap();
	let execution_fees = runtime_api
		.query_weight_to_asset_fee(H256::zero(), xcm_weight.unwrap(), lower_version_native_token)
		.unwrap();
	assert!(execution_fees.is_ok());
}
