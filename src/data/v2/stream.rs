// Copyright (C) 2021 The apca Developers
// SPDX-License-Identifier: GPL-3.0-or-later

use std::borrow::Borrow as _;
use std::borrow::Cow;
use std::cmp::Ordering;

use chrono::DateTime;
use chrono::Utc;

use num_decimal::Num;

use serde::ser::Serializer;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Error as JsonError;

use websocket_util::subscribe;
use websocket_util::tungstenite::Error as WebSocketError;

use crate::websocket::MessageResult;
use crate::Str;


/// Serialize a `Symbol::Symbol` variant.
fn symbol_to_str<S>(symbol: &Str, serializer: S) -> Result<S::Ok, S::Error>
where
  S: Serializer,
{
  serializer.serialize_str(symbol)
}


/// Serialize a `Symbol::All` variant.
fn symbol_all<S>(serializer: S) -> Result<S::Ok, S::Error>
where
  S: Serializer,
{
  serializer.serialize_str("*")
}


/// A symbol for which market data can be received.
#[derive(Clone, Debug, PartialEq, PartialOrd, Serialize)]
#[serde(untagged)]
pub enum Symbol {
  /// A symbol for a specific equity.
  #[serde(serialize_with = "symbol_to_str")]
  Symbol(Str),
  /// A "wildcard" symbol, representing all available equities.
  #[serde(serialize_with = "symbol_all")]
  All,
}

impl From<&'static str> for Symbol {
  fn from(symbol: &'static str) -> Self {
    if symbol == "*" {
      Symbol::All
    } else {
      Symbol::Symbol(Cow::from(symbol))
    }
  }
}

impl From<String> for Symbol {
  fn from(symbol: String) -> Self {
    if symbol == "*" {
      Symbol::All
    } else {
      Symbol::Symbol(Cow::from(symbol))
    }
  }
}


/// A slice/vector of [`Symbol`] objects.
pub type Symbols = Cow<'static, [Symbol]>;


/// Check whether a slice of `Symbol` objects is normalized.
///
/// Such a slice is normalized if:
/// - it is empty or
/// - it contains a single element `Symbol::All` or
/// - it does not contain `Symbol::All` and all symbols are lexically
///   ordered
fn is_normalized(symbols: &[Symbol]) -> bool {
  // The body here is effectively a copy of `Iterator::is_sorted_by`. We
  // should use that once it's stable.

  #[inline]
  fn check<'a>(last: &'a mut &'a Symbol) -> impl FnMut(&'a Symbol) -> bool + 'a {
    move |curr| {
      if let Some(Ordering::Greater) | None = PartialOrd::partial_cmp(last, &curr) {
        return false
      }
      *last = curr;
      true
    }
  }

  if symbols.len() > 1 && symbols.contains(&Symbol::All) {
    return false
  }

  let mut it = symbols.iter();
  let mut last = match it.next() {
    Some(e) => e,
    None => return true,
  };

  it.all(check(&mut last))
}


/// Normalize a list of symbols.
fn normalize(symbols: Symbols) -> Symbols {
  fn normalize_now(symbols: Symbols) -> Symbols {
    if symbols.contains(&Symbol::All) {
      Cow::from([Symbol::All].as_ref())
    } else {
      let mut symbols = symbols.into_owned();
      // Unwrapping here is fine, as we know that there is no
      // `Symbol::All` variant in the list and so we cannot encounter
      // variants that are not comparable.
      symbols.sort_by(|x, y| x.partial_cmp(y).unwrap());
      symbols.dedup();
      Cow::from(symbols)
    }
  }

  if !is_normalized((*symbols).borrow()) {
    let symbols = normalize_now(symbols);
    debug_assert!(is_normalized(&symbols));
    symbols
  } else {
    symbols
  }
}


/// Aggregate data for an equity.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Bar {
  /// The bar's symbol.
  #[serde(rename = "S")]
  pub symbol: String,
  /// The bar's open price.
  #[serde(rename = "o")]
  pub open_price: Num,
  /// The bar's high price.
  #[serde(rename = "h")]
  pub high_price: Num,
  /// The bar's low price.
  #[serde(rename = "l")]
  pub low_price: Num,
  /// The bar's close price.
  #[serde(rename = "c")]
  pub close_price: Num,
  /// The bar's volume.
  #[serde(rename = "v")]
  pub volume: u64,
  /// The bar's time stamp.
  #[serde(rename = "t")]
  pub timestamp: DateTime<Utc>,
}


/// An error as reported by the Alpaca Data API.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ApiError {
  /// The error code being reported.
  #[serde(rename = "code")]
  pub code: u64,
  /// A message providing more details about the error.
  #[serde(rename = "msg")]
  pub message: String,
}


/// An enum representing the different messages we may receive over our
/// websocket channel.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "T")]
#[allow(clippy::large_enum_variant)]
pub enum DataMessage {
  /// A variant representing aggregate data for a given symbol.
  #[serde(rename = "b")]
  Bar(Bar),
  /// A control message indicating that the last operation was
  /// successful.
  #[serde(rename = "success")]
  Success,
  /// An error reported by the Alpaca Data API.
  #[serde(rename = "error")]
  Error(ApiError),
}


/// A data item as received over the our websocket channel.
#[derive(Debug)]
pub enum Data {
  /// A variant representing aggregate data for a given symbol.
  Bar(Bar),
}


/// An enumeration of the supported control messages.
#[derive(Debug)]
pub enum ControlMessage {
  /// A control message indicating that the last operation was
  /// successful.
  Success,
  /// An error reported by the Alpaca Data API.
  Error(ApiError),
}


/// A websocket message that we tried to parse.
type ParsedMessage = MessageResult<Result<DataMessage, JsonError>, WebSocketError>;

impl subscribe::Message for ParsedMessage {
  type UserMessage = Result<Result<Data, JsonError>, WebSocketError>;
  type ControlMessage = ControlMessage;

  fn classify(self) -> subscribe::Classification<Self::UserMessage, Self::ControlMessage> {
    match self {
      MessageResult::Ok(Ok(message)) => match message {
        DataMessage::Bar(bar) => subscribe::Classification::UserMessage(Ok(Ok(Data::Bar(bar)))),
        DataMessage::Success => subscribe::Classification::ControlMessage(ControlMessage::Success),
        DataMessage::Error(error) => {
          subscribe::Classification::ControlMessage(ControlMessage::Error(error))
        },
      },
      // JSON errors are directly passed through.
      MessageResult::Ok(Err(err)) => subscribe::Classification::UserMessage(Ok(Err(err))),
      // WebSocket errors are also directly pushed through.
      MessageResult::Err(err) => subscribe::Classification::UserMessage(Err(err)),
    }
  }

  #[inline]
  fn is_error(user_message: &Self::UserMessage) -> bool {
    // Both outer `WebSocketError` and inner `JsonError` errors
    // constitute errors in our sense. Note however than an API error
    // does not. It's just a regular control message from our
    // perspective.
    user_message
      .as_ref()
      .map(|result| result.is_err())
      .unwrap_or(true)
  }
}


/// A type wrapping an instance of [`Symbols`] that is guaranteed to be
/// normalized.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Normalized(Symbols);

impl From<Symbols> for Normalized {
  fn from(symbols: Symbols) -> Self {
    Self(normalize(symbols))
  }
}

impl From<Vec<String>> for Normalized {
  fn from(symbols: Vec<String>) -> Self {
    Self(normalize(Cow::from(
      IntoIterator::into_iter(symbols)
        .map(Symbol::from)
        .collect::<Vec<_>>(),
    )))
  }
}

impl<const N: usize> From<[&'static str; N]> for Normalized {
  fn from(symbols: [&'static str; N]) -> Self {
    Self(normalize(Cow::from(
      IntoIterator::into_iter(symbols)
        .map(Symbol::from)
        .collect::<Vec<_>>(),
    )))
  }
}


/// A type defining the market data a client intends to subscribe to.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MarketData {
  /// The aggregate bars to subscribe to.
  pub bars: Normalized,
}

impl MarketData {
  /// A convenience function for setting the [`bars`][MarketData::bars]
  /// member.
  pub fn set_bars<N>(&mut self, symbols: N)
  where
    N: Into<Normalized>,
  {
    self.bars = symbols.into();
  }
}


#[cfg(test)]
mod tests {
  use super::*;

  use chrono::TimeZone as _;

  use serde_json::from_str as json_from_str;


  /// Check that we can deserialize the [`DataMessage::Bar`] variant.
  #[test]
  fn parse_bar() {
    let json = r#"{
  "T": "b",
  "S": "SPY",
  "o": 388.985,
  "h": 389.13,
  "l": 388.975,
  "c": 389.12,
  "v": 49378,
  "t": "2021-02-22T19:15:00Z"
}"#;

    let message = json_from_str::<DataMessage>(json).unwrap();
    let bar = match message {
      DataMessage::Bar(bar) => bar,
      _ => panic!("Decoded unexpected message variant: {:?}", message),
    };
    assert_eq!(bar.symbol, "SPY");
    assert_eq!(bar.open_price, Num::new(388985, 1000));
    assert_eq!(bar.high_price, Num::new(38913, 100));
    assert_eq!(bar.low_price, Num::new(388975, 1000));
    assert_eq!(bar.close_price, Num::new(38912, 100));
    assert_eq!(bar.volume, 49378);
    assert_eq!(
      bar.timestamp,
      Utc.ymd(2021, 2, 22).and_hms_milli(19, 15, 0, 0)
    );
  }

  /// Check that we can deserialize the [`DataMessage::Success`] variant.
  #[test]
  fn parse_success() {
    let json = r#"{"T":"success","msg":"authenticated"}"#;
    let message = json_from_str::<DataMessage>(json).unwrap();
    let () = match message {
      DataMessage::Success => (),
      _ => panic!("Decoded unexpected message variant: {:?}", message),
    };
  }

  /// Check that we can deserialize the [`DataMessage::Error`] variant.
  #[test]
  fn parse_error() {
    let json = r#"{"T":"error","code":400,"msg":"invalid syntax"}"#;
    let message = json_from_str::<DataMessage>(json).unwrap();
    let error = match message {
      DataMessage::Error(error) => error,
      _ => panic!("Decoded unexpected message variant: {:?}", message),
    };

    assert_eq!(error.code, 400);
    assert_eq!(error.message, "invalid syntax");

    let json = r#"{"T":"error","code":500,"msg":"internal error"}"#;
    let message = json_from_str::<DataMessage>(json).unwrap();
    let error = match message {
      DataMessage::Error(error) => error,
      _ => panic!("Decoded unexpected message variant: {:?}", message),
    };

    assert_eq!(error.code, 500);
    assert_eq!(error.message, "internal error");
  }

  /// Check that we can normalize `Symbol` slices.
  #[test]
  fn normalize_subscriptions() {
    let subscriptions = [Symbol::All];
    assert!(is_normalized(&subscriptions));

    let subscriptions = [Symbol::Symbol("MSFT".into()), Symbol::Symbol("SPY".into())];
    assert!(is_normalized(&subscriptions));

    let mut subscriptions = Cow::from(vec![
      Symbol::Symbol("SPY".into()),
      Symbol::Symbol("MSFT".into()),
    ]);
    assert!(!is_normalized(&subscriptions));
    subscriptions = normalize(subscriptions);
    assert!(is_normalized(&subscriptions));

    let expected = [Symbol::Symbol("MSFT".into()), Symbol::Symbol("SPY".into())];
    assert_eq!(subscriptions.as_ref(), expected.as_ref());

    let mut subscriptions = Cow::from(vec![
      Symbol::Symbol("SPY".into()),
      Symbol::All,
      Symbol::Symbol("MSFT".into()),
    ]);
    assert!(!is_normalized(&subscriptions));
    subscriptions = normalize(subscriptions);
    assert!(is_normalized(&subscriptions));

    let expected = [Symbol::All];
    assert_eq!(subscriptions.as_ref(), expected.as_ref());
  }
}