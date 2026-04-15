use std::convert::Infallible;
use std::pin::Pin;
use std::time::Duration;

use anyhow::anyhow;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use fedimint_client::ClientHandleArc;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_ln_client::{LightningClientModule, LnReceiveState};
use futures_util::stream::Stream;
use serde::Deserialize;
use tokio_stream::wrappers::IntervalStream;
use tracing::{error, info, warn};

use crate::core::operations::{OperationStatus, PaymentOperation};
use crate::core::{InvoiceStatus, SettlementInfo};
use crate::error::AppError;
use crate::state::AppState;

/// Invoice status update for streaming
#[derive(Debug, serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InvoiceStatusUpdate {
    pub invoice_id: String,
    pub operation_id: OperationId,
    pub status: InvoiceStatus,
    pub settlement: Option<SettlementInfo>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamQuery {
    pub federation_id: FederationId,
    /// Heartbeat interval in seconds (default: 30)
    pub heartbeat_interval: Option<u64>,
    /// Stream timeout in seconds (default: 600)
    pub timeout_seconds: Option<u64>,
}

/// Create a unified invoice status stream using fedimint's native
/// subscribe_ln_receive
async fn create_unified_invoice_stream(
    client: ClientHandleArc,
    operation_id: OperationId,
    tracked_operation: Option<PaymentOperation>,
    heartbeat_interval: Duration,
    timeout: Duration,
) -> Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> {
    use futures_util::stream::{self, StreamExt};

    let invoice_amount_msat = client
        .operation_log()
        .get_operation(operation_id)
        .await
        .and_then(|op| {
            // Extract amount from operation metadata if available
            op.meta::<serde_json::Value>()
                .get("amount")
                .and_then(|v| v.as_u64())
        })
        .unwrap_or(0); // Default to 0 if not found

    let tracked_protocol = tracked_operation
        .as_ref()
        .and_then(|operation| operation.protocol.as_deref());
    let tracked_is_terminal = tracked_operation
        .as_ref()
        .map(|operation| operation.status.is_terminal())
        .unwrap_or(false);

    // Create heartbeat stream for connection keepalive
    let heartbeat_stream = IntervalStream::new(tokio::time::interval(heartbeat_interval))
        .map(|_| Ok::<_, Infallible>(Event::default().event("heartbeat").data("ping")));

    // Create timeout stream using futures_util for consistency
    let timeout_stream = stream::once(async move {
        tokio::time::sleep(timeout).await;
        warn!(
            operation_id = ?operation_id,
            timeout_secs = timeout.as_secs(),
            "Unified invoice stream timed out"
        );
        Ok::<_, Infallible>(Event::default().event("timeout").data(format!(
            "{{\"message\":\"Stream timed out after {} seconds\",\"timeout_seconds\":{}}}",
            timeout.as_secs(),
            timeout.as_secs()
        )))
    });

    if tracked_protocol == Some("lnv2") || tracked_is_terminal {
        let tracked_stream = stream::once(async move {
            match tracked_operation {
                Some(operation) => {
                    let update = tracked_operation_to_status_update(
                        operation,
                        operation_id,
                        invoice_amount_msat,
                    );

                    match serde_json::to_string(&update) {
                        Ok(json_data) => Ok::<_, Infallible>(
                            Event::default().event("invoice_update").data(json_data),
                        ),
                        Err(e) => {
                            error!(
                                operation_id = ?operation_id,
                                error = ?e,
                                "Failed to serialize tracked invoice update"
                            );
                            Ok::<_, Infallible>(
                                Event::default()
                                    .event("error")
                                    .data(format!("Serialization error: {}", e)),
                            )
                        }
                    }
                }
                None => Ok::<_, Infallible>(
                    Event::default()
                        .event("error")
                        .data("Tracked operation not found"),
                ),
            }
        });

        return Box::pin(stream::select_all(vec![
            heartbeat_stream.boxed(),
            tracked_stream.boxed(),
            timeout_stream.boxed(),
        ]));
    }

    let lightning_module = match client.get_first_module::<LightningClientModule>() {
        Ok(module) => module,
        Err(e) => {
            error!(
                operation_id = ?operation_id,
                error = ?e,
                "Failed to get lightning module for unified streaming"
            );
            return tokio_stream::empty().boxed();
        }
    };

    // Use fedimint's native subscribe_ln_receive for real-time monitoring
    let updates_stream = match lightning_module.subscribe_ln_receive(operation_id).await {
        Ok(stream) => stream.into_stream(),
        Err(e) => {
            error!(
                operation_id = ?operation_id,
                error = ?e,
                "Failed to subscribe to fedimint native invoice updates"
            );
            return tokio_stream::empty().boxed();
        }
    };

    info!(
        operation_id = ?operation_id,
        timeout_secs = timeout.as_secs(),
        heartbeat_interval_secs = heartbeat_interval.as_secs(),
        "Started unified invoice stream using fedimint native behavior"
    );

    // Convert fedimint LnReceiveState updates to unified SSE events
    let invoice_updates_stream = updates_stream.map(move |ln_state| {
        let updated_at = Utc::now();
        let (status, settlement_info) =
            fedimint_state_to_unified_status(ln_state, updated_at, invoice_amount_msat);

        let update = InvoiceStatusUpdate {
            invoice_id: format!("inv_{:?}", operation_id), // Generate consistent invoice_id
            operation_id,
            status,
            settlement: settlement_info,
            updated_at,
        };

        match serde_json::to_string(&update) {
            Ok(json_data) => {
                info!(
                    operation_id = ?operation_id,
                    status = ?update.status,
                    "Sending unified invoice status update via native fedimint stream"
                );
                Ok::<_, Infallible>(Event::default().event("invoice_update").data(json_data))
            }
            Err(e) => {
                error!(
                    operation_id = ?operation_id,
                    error = ?e,
                    "Failed to serialize unified invoice update"
                );
                Ok::<_, Infallible>(
                    Event::default()
                        .event("error")
                        .data(format!("Serialization error: {}", e)),
                )
            }
        }
    });

    // Select from all streams concurrently
    let combined_stream = stream::select_all(vec![
        heartbeat_stream.boxed(),
        invoice_updates_stream.boxed(),
        timeout_stream.boxed(),
    ]);

    Box::pin(combined_stream)
}

/// Convert fedimint LnReceiveState to unified status representation
fn fedimint_state_to_unified_status(
    ln_state: LnReceiveState,
    updated_at: chrono::DateTime<Utc>,
    invoice_amount_msat: u64,
) -> (InvoiceStatus, Option<SettlementInfo>) {
    match ln_state {
        LnReceiveState::Created => (InvoiceStatus::Created, None),
        LnReceiveState::WaitingForPayment { .. } => (InvoiceStatus::Pending, None),
        LnReceiveState::Claimed => {
            // NOTE: Fedimint's LnReceiveState::Claimed doesn't include settlement details
            // The actual amount received might differ from invoice amount due to fees.
            // Using the invoice amount from operation metadata as a reasonable
            // approximation. This ensures real-time monitoring receives
            // meaningful data rather than 0.
            let settlement_info = SettlementInfo {
                amount_received_msat: if invoice_amount_msat > 0 {
                    invoice_amount_msat
                } else {
                    // Log warning if amount is not available
                    warn!("Invoice amount not found in operation metadata for stream, using 0");
                    0
                },
                settled_at: updated_at, // Using update time as approximation
                preimage: None,         // Not exposed in current API
                gateway_fee_msat: None, // Not exposed in current API
            };
            (
                InvoiceStatus::Claimed {
                    amount_received_msat: settlement_info.amount_received_msat,
                    settled_at: settlement_info.settled_at,
                },
                Some(settlement_info),
            )
        }
        LnReceiveState::Canceled { reason } => (
            InvoiceStatus::Canceled {
                reason: reason.to_string(),
                canceled_at: updated_at,
            },
            None,
        ),
        LnReceiveState::Funded => (InvoiceStatus::Pending, None),
        LnReceiveState::AwaitingFunds => (InvoiceStatus::Pending, None),
        // Note: All LnReceiveState variants are now explicitly handled
        // If new variants are added to fedimint, compilation will fail here
    }
}

fn tracked_operation_to_status_update(
    operation: PaymentOperation,
    operation_id: OperationId,
    fallback_amount_msat: u64,
) -> InvoiceStatusUpdate {
    let updated_at = operation.updated_at;
    let amount_received_msat = operation
        .amount_msat
        .map(|amount| amount.msats)
        .filter(|amount| *amount > 0)
        .unwrap_or(fallback_amount_msat);

    let (status, settlement) = match operation.status {
        OperationStatus::Created => (InvoiceStatus::Created, None),
        OperationStatus::Pending => (InvoiceStatus::Pending, None),
        OperationStatus::Succeeded => {
            let settlement_info = SettlementInfo {
                amount_received_msat,
                settled_at: updated_at,
                preimage: None,
                gateway_fee_msat: operation.fee_msat,
            };

            (
                InvoiceStatus::Claimed {
                    amount_received_msat,
                    settled_at: updated_at,
                },
                Some(settlement_info),
            )
        }
        OperationStatus::TimedOut => (
            InvoiceStatus::Expired {
                expired_at: updated_at,
            },
            None,
        ),
        OperationStatus::Failed | OperationStatus::Refunded => (
            InvoiceStatus::Canceled {
                reason: operation
                    .last_error
                    .unwrap_or_else(|| "Invoice canceled".to_string()),
                canceled_at: updated_at,
            },
            None,
        ),
    };

    InvoiceStatusUpdate {
        invoice_id: format!("inv_{:?}", operation_id),
        operation_id,
        status,
        settlement,
        updated_at,
    }
}

/// Unified invoice stream endpoint - supports both operation_id and invoice_id
#[axum_macros::debug_handler]
pub async fn handle_operation_stream(
    State(state): State<AppState>,
    Path(operation_id_str): Path<String>,
    Query(query): Query<StreamQuery>,
) -> Result<Response, AppError> {
    let operation_id = operation_id_str.parse::<OperationId>().map_err(|e| {
        AppError::new(
            StatusCode::BAD_REQUEST,
            anyhow!("Invalid operation ID: {}", e),
        )
    })?;

    let client = state.get_client(query.federation_id).await?;
    let tracked_operation = state.core.get_tracked_operation(&operation_id).await;
    let heartbeat_interval = Duration::from_secs(query.heartbeat_interval.unwrap_or(30));
    let timeout = Duration::from_secs(query.timeout_seconds.unwrap_or(600));

    info!(
        operation_id = ?operation_id,
        federation_id = %query.federation_id,
        heartbeat_interval_secs = heartbeat_interval.as_secs(),
        timeout_secs = timeout.as_secs(),
        "Starting unified invoice stream for operation"
    );

    let stream = create_unified_invoice_stream(
        client,
        operation_id,
        tracked_operation,
        heartbeat_interval,
        timeout,
    )
    .await;

    let sse = Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(heartbeat_interval)
            .text("keep-alive"),
    );

    Ok(sse.into_response())
}

/// Multi-invoice event stream from the event bus
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventStreamQuery {
    pub federation_id: FederationId,
    /// Filter by specific invoice/operation IDs (optional)
    pub filter_ids: Option<Vec<String>>,
    /// Heartbeat interval in seconds (default: 30)
    pub heartbeat_interval: Option<u64>,
}

/// Global event stream for all invoices in a federation
#[axum_macros::debug_handler]
pub async fn handle_global_event_stream(
    State(state): State<AppState>,
    Query(query): Query<EventStreamQuery>,
) -> Result<Response, AppError> {
    let mut event_receiver = state.event_bus().subscribe();
    let federation_id = query.federation_id.to_string();
    let heartbeat_interval = Duration::from_secs(query.heartbeat_interval.unwrap_or(30));
    let filter_ids = query.filter_ids.unwrap_or_default();

    info!(
        federation_id = %federation_id,
        filter_count = filter_ids.len(),
        "Starting unified global invoice event stream"
    );

    let stream = async_stream::stream! {
        let mut heartbeat_interval = tokio::time::interval(heartbeat_interval);

        loop {
            tokio::select! {
                _ = heartbeat_interval.tick() => {
                    yield Ok::<_, Infallible>(Event::default()
                        .event("heartbeat")
                        .data("ping"));
                }
                event_result = event_receiver.recv() => {
                    match event_result {
                        Ok(event) => {
                            // Filter events for the requested federation
                            let should_send = match &event {
                                crate::events::FmcdEvent::InvoiceCreated { federation_id: fid, .. } => {
                                    fid == &federation_id && (filter_ids.is_empty() ||
                                        filter_ids.iter().any(|id| event.contains_id(id)))
                                },
                                crate::events::FmcdEvent::InvoicePaid { federation_id: fid, .. } => {
                                    fid == &federation_id && (filter_ids.is_empty() ||
                                        filter_ids.iter().any(|id| event.contains_id(id)))
                                },
                                crate::events::FmcdEvent::InvoiceExpired { federation_id: fid, .. } => {
                                    fid == &federation_id && (filter_ids.is_empty() ||
                                        filter_ids.iter().any(|id| event.contains_id(id)))
                                },
                                _ => false,
                            };

                            if should_send {
                                match serde_json::to_string(&event) {
                                    Ok(json_data) => {
                                        yield Ok::<_, Infallible>(Event::default()
                                            .event("invoice_event")
                                            .data(json_data));
                                    }
                                    Err(e) => {
                                        warn!(error = ?e, "Failed to serialize unified event");
                                        yield Ok::<_, Infallible>(Event::default()
                                            .event("error")
                                            .data(format!("Serialization error: {}", e)));
                                    }
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped = skipped, "Unified event stream lagged");
                            yield Ok::<_, Infallible>(Event::default()
                                .event("warning")
                                .data(format!("Stream lagged, {} events skipped", skipped)));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            info!("Unified event bus closed, ending stream");
                            break;
                        }
                    }
                }
            }
        }
    };

    let sse = Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(heartbeat_interval)
            .text("keep-alive"),
    );

    Ok(sse.into_response())
}

// TODO: Add helper method to FmcdEvent for ID filtering
trait EventIdFilter {
    fn contains_id(&self, id: &str) -> bool;
}

impl EventIdFilter for crate::events::FmcdEvent {
    fn contains_id(&self, id: &str) -> bool {
        match self {
            // Match invoice_id where available, operation_id for InvoicePaid
            crate::events::FmcdEvent::InvoiceCreated { invoice_id, .. } => invoice_id == id,
            crate::events::FmcdEvent::InvoicePaid { operation_id, .. } => operation_id == id,
            crate::events::FmcdEvent::InvoiceExpired { invoice_id, .. } => invoice_id == id,
            _ => false,
        }
    }
}
