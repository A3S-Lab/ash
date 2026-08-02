/// Canonical presentation identifiers for core ASH/1 operations.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Operation {
    Exec,
    Read,
    List,
    Search,
    Patch,
    Fs,
    Batch,
    RefBytes,
    RefLines,
    RefSearch,
    RefRelease,
    RefProject,
    RefMaterialize,
    Snapshot,
    Cancel,
}

impl Operation {
    /// Returns the canonical single-byte ASH/1 presentation identifier.
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            Self::Exec => b'x',
            Self::Read => b'r',
            Self::List => b'l',
            Self::Search => b'g',
            Self::Patch => b'p',
            Self::Fs => b'f',
            Self::Batch => b'b',
            Self::RefBytes => b'/',
            Self::RefLines => b'#',
            Self::RefSearch => b'?',
            Self::RefRelease => b'-',
            Self::RefProject => b'|',
            Self::RefMaterialize => b'>',
            Self::Snapshot => b's',
            Self::Cancel => b'k',
        }
    }

    /// Decodes a canonical ASH/1 presentation identifier.
    #[must_use]
    pub const fn from_id(id: u8) -> Option<Self> {
        match id {
            b'x' => Some(Self::Exec),
            b'r' => Some(Self::Read),
            b'l' => Some(Self::List),
            b'g' => Some(Self::Search),
            b'p' => Some(Self::Patch),
            b'f' => Some(Self::Fs),
            b'b' => Some(Self::Batch),
            b'/' => Some(Self::RefBytes),
            b'#' => Some(Self::RefLines),
            b'?' => Some(Self::RefSearch),
            b'-' => Some(Self::RefRelease),
            b'|' => Some(Self::RefProject),
            b'>' => Some(Self::RefMaterialize),
            b's' => Some(Self::Snapshot),
            b'k' => Some(Self::Cancel),
            _ => None,
        }
    }

    /// Returns this operation's stable bit in the handshake capability mask.
    #[must_use]
    pub const fn mask(self) -> u64 {
        match self {
            Self::Exec => 1 << 0,
            Self::Read => 1 << 1,
            Self::List => 1 << 2,
            Self::Search => 1 << 3,
            Self::Patch => 1 << 4,
            Self::Fs => 1 << 5,
            Self::Batch => 1 << 6,
            Self::RefBytes
            | Self::RefLines
            | Self::RefSearch
            | Self::RefRelease
            | Self::RefProject
            | Self::RefMaterialize => 1 << 7,
            Self::Snapshot => 1 << 8,
            Self::Cancel => 1 << 9,
        }
    }

    /// Retained-result formulas share one negotiated operation-family bit.
    #[must_use]
    pub const fn is_reference_formula(self) -> bool {
        matches!(
            self,
            Self::RefBytes
                | Self::RefLines
                | Self::RefSearch
                | Self::RefRelease
                | Self::RefProject
                | Self::RefMaterialize
        )
    }
}

/// All operation bits defined by ASH/1.0.
pub const ALL_OPERATION_MASK: u64 = (1 << 10) - 1;

#[cfg(test)]
mod tests {
    use super::{ALL_OPERATION_MASK, Operation};

    #[test]
    fn canonical_operation_identifiers_round_trip() {
        let expected = [
            (Operation::Exec, b'x'),
            (Operation::Read, b'r'),
            (Operation::List, b'l'),
            (Operation::Search, b'g'),
            (Operation::Patch, b'p'),
            (Operation::Fs, b'f'),
            (Operation::Batch, b'b'),
            (Operation::RefBytes, b'/'),
            (Operation::RefLines, b'#'),
            (Operation::RefSearch, b'?'),
            (Operation::RefRelease, b'-'),
            (Operation::RefProject, b'|'),
            (Operation::RefMaterialize, b'>'),
            (Operation::Snapshot, b's'),
            (Operation::Cancel, b'k'),
        ];

        for (operation, id) in expected {
            assert_eq!(operation.id(), id);
            assert_eq!(Operation::from_id(id), Some(operation));
        }
        assert_eq!(Operation::from_id(b'~'), None);
        let combined = expected
            .into_iter()
            .map(|(operation, _)| operation.mask())
            .fold(0, |mask, bit| mask | bit);
        assert_eq!(combined, ALL_OPERATION_MASK);
    }
}
