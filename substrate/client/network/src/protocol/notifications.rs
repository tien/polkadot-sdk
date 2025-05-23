// This file is part of Substrate.

// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

//! Implementation of libp2p's `NetworkBehaviour` trait that establishes communications and opens
//! notifications substreams.

pub use self::{
	behaviour::{Notifications, NotificationsOut, ProtocolConfig},
	handler::{NotificationsSink, Ready},
	service::{notification_service, ProtocolHandlePair},
};

pub(crate) use self::service::ProtocolHandle;

mod behaviour;
mod handler;
mod service;
mod tests;
mod upgrade;
