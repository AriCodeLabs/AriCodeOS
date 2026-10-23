// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! What an updater run trusts (ADR 0045), as pure functions so the rules
//! are tested on the host and not only through the loader.

use aricode_ab::{Channel, Keys, Manifest};

/// The keys of an updater run: the release key always; the development
/// channel's key only when this build carries it (`compiled_dev`) **and**
/// the machine's loader trusts that channel (`BootState::
/// loader_dev_channel`, written by AriBoot on every boot). The updater
/// then never accepts an index, nor installs an image, that the loader
/// would refuse.
pub fn trusted_keys(
    stable: [u8; 32],
    compiled_dev: Option<[u8; 32]>,
    loader_trusts_dev: bool,
) -> Keys {
    Keys {
        stable,
        dev: compiled_dev.filter(|_| loader_trusts_dev),
    }
}

/// Whether a downloaded image's verified manifest is what the signed index
/// offered: the same channel, version (build number) and commit. The
/// manifest's own signature already bound its channel to its key; this
/// binds the image to the index, so a development index cannot install a
/// stable image (or another build) in its name.
pub fn image_matches_offer(
    manifest: &Manifest,
    channel: Channel,
    version: u32,
    commit: Option<&str>,
) -> bool {
    manifest.channel == channel
        && manifest.version == version
        && manifest.commit.as_ref().map(|c| c.as_str()) == commit
}

#[cfg(test)]
mod tests {
    use super::*;
    use aricode_ab::Commit;

    #[test]
    fn the_development_key_needs_the_build_and_the_loader() {
        let (stable, dev) = ([1u8; 32], [2u8; 32]);
        assert_eq!(trusted_keys(stable, Some(dev), true).dev, Some(dev));
        // A loader that does not trust the channel (byte 16 zero, or a
        // loader before ADR 0045): the key stays unused.
        assert_eq!(trusted_keys(stable, Some(dev), false).dev, None);
        // A build without the key.
        assert_eq!(trusted_keys(stable, None, true).dev, None);
        assert_eq!(trusted_keys(stable, None, false), Keys::stable_only(stable));
        assert_eq!(trusted_keys(stable, Some(dev), false).stable, stable);
    }

    #[test]
    fn the_image_must_be_the_one_the_index_offered() {
        let dev = Manifest {
            channel: Channel::Dev,
            commit: Commit::parse("0123abc"),
            ..Manifest::stable(5, [0; 32], [0; 32])
        };
        assert!(image_matches_offer(&dev, Channel::Dev, 5, Some("0123abc")));
        assert!(!image_matches_offer(&dev, Channel::Dev, 6, Some("0123abc")));
        assert!(!image_matches_offer(&dev, Channel::Dev, 5, Some("4567def")));
        assert!(!image_matches_offer(&dev, Channel::Dev, 5, None));
        assert!(!image_matches_offer(&dev, Channel::Stable, 5, None));
        // A development index pointing at a stable image of the same number.
        let stable = Manifest::stable(5, [0; 32], [0; 32]);
        assert!(!image_matches_offer(
            &stable,
            Channel::Dev,
            5,
            Some("0123abc")
        ));
        assert!(image_matches_offer(&stable, Channel::Stable, 5, None));
    }
}
