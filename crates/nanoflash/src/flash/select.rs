//! Choosing the boards of a manifest that suit a device.

use alloc::string::ToString;
use alloc::vec::Vec;

use crate::error::Error;
use crate::manifest::{Board, Manifest, Protocol, ResetSequence};
use crate::transport::UsbIdentity;

/// The protocol the USB vendor of `identity` speaks, when it is known.
pub fn protocol_for_identity(identity: &UsbIdentity) -> Option<Protocol> {
    match identity.vendor {
        0x303a => Some(Protocol::Espressif),
        0x2341 => Some(Protocol::Samba),
        _ => None,
    }
}

/// The boards that may be the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection<'m> {
    /// The protocol every board speaks.
    pub protocol: Protocol,
    /// The candidate boards, in manifest order.
    pub boards: Vec<&'m Board>,
    /// The reset the boards share, when they all agree.
    pub reset: Option<ResetSequence>,
}

/// Whether a board without a USB identity may sit behind a serial bridge.
fn suits_bridge(board: &Board) -> bool {
    matches!(
        board.reset,
        None | Some(ResetSequence::EspressifClassic)
            | Some(ResetSequence::None)
            | Some(ResetSequence::Custom(_))
    ) && board.application_devices.is_empty()
        && board.bootloader_devices.is_empty()
}

/// Selects the boards of `manifest` for a device.
///
/// With `board_identifier`, exactly that board. Otherwise the boards that list the device, or,
/// when none does or the device has no identity, the boards that suit a serial bridge.
pub fn select<'m>(
    manifest: &'m Manifest,
    identity: Option<&UsbIdentity>,
    board_identifier: Option<&str>,
) -> Result<Selection<'m>, Error> {
    let boards: Vec<&Board> = match board_identifier {
        Some(identifier) => {
            let board = manifest
                .boards
                .iter()
                .find(|board| board.identifier == identifier)
                .ok_or_else(|| Error::UnknownBoard {
                    identifier: identifier.to_string(),
                    known: manifest
                        .boards
                        .iter()
                        .map(|board| board.identifier.clone())
                        .collect(),
                })?;
            alloc::vec![board]
        }
        None => {
            let listed: Vec<&Board> = match identity {
                Some(identity) => manifest
                    .boards
                    .iter()
                    .filter(|board| {
                        board
                            .application_devices
                            .iter()
                            .chain(&board.bootloader_devices)
                            .any(|filter| filter.matches(identity))
                    })
                    .collect(),
                None => Vec::new(),
            };
            if listed.is_empty() {
                manifest
                    .boards
                    .iter()
                    .filter(|board| suits_bridge(board))
                    .collect()
            } else {
                listed
            }
        }
    };
    let Some(first) = boards.first() else {
        return Err(Error::NoMatchingBoard {
            identity: identity.cloned(),
        });
    };
    let mut protocols: Vec<Protocol> = boards.iter().map(|board| board.protocol).collect();
    protocols.sort();
    protocols.dedup();
    if protocols.len() > 1 {
        return Err(Error::SeveralProtocols { protocols });
    }
    let reset = if boards.iter().all(|board| board.reset == first.reset) {
        first.reset.clone()
    } else {
        None
    };
    Ok(Selection {
        protocol: first.protocol,
        boards,
        reset,
    })
}

impl<'m> Selection<'m> {
    /// The board whose chip is `chip`, as the backend detected it.
    pub fn pick(&self, chip: &str) -> Result<&'m Board, Error> {
        let mut matching = self
            .boards
            .iter()
            .copied()
            .filter(|board| board.matches_chip(chip));
        let Some(first) = matching.next() else {
            return Err(Error::WrongChip {
                detected: chip.to_string(),
                expected: self.boards.iter().map(|board| board.chip.clone()).collect(),
            });
        };
        if matching.next().is_none() {
            return Ok(first);
        }
        Err(Error::SeveralBoards {
            chip: chip.to_string(),
            boards: self
                .boards
                .iter()
                .filter(|board| board.matches_chip(chip))
                .map(|board| board.identifier.clone())
                .collect(),
        })
    }
}

#[cfg(all(test, feature = "testing", feature = "json"))]
mod tests {
    use super::*;

    const WEB_TOOLS: &str = include_str!("../../tests/boards/esp-web-tools.json");

    fn identity(vendor: u16, product: u16) -> UsbIdentity {
        UsbIdentity {
            vendor,
            product,
            serial_number: None,
        }
    }

    fn two_boards() -> Manifest {
        Manifest::from_toml(
            r#"
[[board]]
identifier = "s3"
protocol = "espressif"
chip = "esp32-s3"
application-devices = [{ vendor = 0x303a, product = 0x1001 }]
[[board.image]]
offset = 0
path = "a.bin"

[[board]]
identifier = "samba"
protocol = "samba"
chip = "samd21"
bootloader-devices = [{ vendor = 0x2341, product = 0x805a }]
[[board.image]]
offset = 0
path = "a.bin"
"#,
        )
        .unwrap()
    }

    #[test]
    fn usb_identity_picks_the_board() {
        let manifest = two_boards();
        let selection = select(&manifest, Some(&identity(0x303a, 0x1001)), None).unwrap();
        assert_eq!(selection.protocol, Protocol::Espressif);
        assert_eq!(selection.boards.len(), 1);
        assert_eq!(selection.boards[0].identifier, "s3");
        let selection = select(&manifest, Some(&identity(0x2341, 0x805a)), None).unwrap();
        assert_eq!(selection.protocol, Protocol::Samba);
    }

    #[test]
    fn bridges_fall_back_to_classic_boards() {
        let manifest = Manifest::from_toml(
            r#"
[[board]]
identifier = "classic"
protocol = "espressif"
chip = "esp32"
reset = "espressif-classic"
[[board.image]]
offset = 0
path = "a.bin"

[[board]]
identifier = "unset"
protocol = "espressif"
chip = "esp32-s3"
[[board.image]]
offset = 0
path = "a.bin"

[[board]]
identifier = "jtag"
protocol = "espressif"
chip = "esp32-c3"
reset = "espressif-usb-serial-jtag"
[[board.image]]
offset = 0
path = "a.bin"

[[board]]
identifier = "listed"
protocol = "espressif"
chip = "esp32-c6"
application-devices = [{ vendor = 0x303a }]
[[board.image]]
offset = 0
path = "a.bin"
"#,
        )
        .unwrap();
        let selection = select(&manifest, Some(&identity(0x10c4, 0xea60)), None).unwrap();
        let identifiers: Vec<&str> = selection
            .boards
            .iter()
            .map(|board| board.identifier.as_str())
            .collect();
        assert_eq!(identifiers, ["classic", "unset"]);
        assert_eq!(selection.reset, None);
        let without = select(&manifest, None, None).unwrap();
        assert_eq!(without.boards.len(), 2);
        let only = select(&manifest, None, Some("classic")).unwrap();
        assert_eq!(only.reset, Some(ResetSequence::EspressifClassic));
        let error = select(&manifest, None, Some("nope")).unwrap_err();
        assert!(matches!(error, Error::UnknownBoard { ref known, .. } if known.len() == 4));
    }

    #[test]
    fn several_protocols_need_an_identifier() {
        let manifest = Manifest::from_toml(
            r#"
[[board]]
identifier = "a"
protocol = "espressif"
chip = "esp32"
[[board.image]]
offset = 0
path = "a.bin"

[[board]]
identifier = "b"
protocol = "samba"
chip = "samd21"
reset = "none"
[[board.image]]
offset = 0
path = "a.bin"
"#,
        )
        .unwrap();
        assert_eq!(
            select(&manifest, None, None).unwrap_err(),
            Error::SeveralProtocols {
                protocols: alloc::vec![Protocol::Espressif, Protocol::Samba]
            }
        );
        assert!(select(&manifest, None, Some("a")).is_ok());
        assert_eq!(
            select(&two_boards(), Some(&identity(0x0483, 0xdf11)), None).unwrap_err(),
            Error::NoMatchingBoard {
                identity: Some(identity(0x0483, 0xdf11))
            }
        );
    }

    #[test]
    fn pick_by_detected_chip() {
        let manifest = Manifest::from_json(WEB_TOOLS).unwrap();
        let selection = select(&manifest, None, None).unwrap();
        assert_eq!(selection.boards.len(), 2);
        assert_eq!(selection.pick("esp32-s3").unwrap().chip, "esp32-s3");
        let error = selection.pick("esp32-c6").unwrap_err();
        assert_eq!(
            error,
            Error::WrongChip {
                detected: "esp32-c6".into(),
                expected: alloc::vec!["esp32".into(), "esp32-s3".into()],
            }
        );
        assert_eq!(error.class(), crate::ErrorClass::Device);
        let duplicated = Manifest::from_toml(
            r#"
[[board]]
identifier = "a"
protocol = "espressif"
chip = "esp32"
[[board.image]]
offset = 0
path = "a.bin"
[[board]]
identifier = "b"
protocol = "espressif"
chip = "ESP32"
[[board.image]]
offset = 0
path = "a.bin"
"#,
        )
        .unwrap();
        assert_eq!(
            select(&duplicated, None, None)
                .unwrap()
                .pick("esp32")
                .unwrap_err(),
            Error::SeveralBoards {
                chip: "esp32".into(),
                boards: alloc::vec!["a".into(), "b".into()],
            }
        );
    }

    #[test]
    fn identity_table() {
        assert_eq!(
            protocol_for_identity(&identity(0x303a, 0x1001)),
            Some(Protocol::Espressif)
        );
        assert_eq!(
            protocol_for_identity(&identity(0x2341, 0x805a)),
            Some(Protocol::Samba)
        );
        assert_eq!(protocol_for_identity(&identity(0x0483, 0xdf11)), None);
    }
}
