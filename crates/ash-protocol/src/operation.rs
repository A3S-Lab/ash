/// Stable presentation identifiers for core ASH/1 operations.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Operation {
    Exec,
    Read,
    List,
    Search,
    Patch,
    Fs,
    Batch,
    Ref,
    Snapshot,
    Cancel,
}

impl Operation {
    /// Returns the stable single-byte ASH/1 presentation identifier.
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
            Self::Ref => b'h',
            Self::Snapshot => b's',
            Self::Cancel => b'k',
        }
    }

    /// Decodes a stable ASH/1 presentation identifier.
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
            b'h' => Some(Self::Ref),
            b's' => Some(Self::Snapshot),
            b'k' => Some(Self::Cancel),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Operation;

    #[test]
    fn operation_identifiers_are_stable_and_round_trip() {
        let expected = [
            (Operation::Exec, b'x'),
            (Operation::Read, b'r'),
            (Operation::List, b'l'),
            (Operation::Search, b'g'),
            (Operation::Patch, b'p'),
            (Operation::Fs, b'f'),
            (Operation::Batch, b'b'),
            (Operation::Ref, b'h'),
            (Operation::Snapshot, b's'),
            (Operation::Cancel, b'k'),
        ];

        for (operation, id) in expected {
            assert_eq!(operation.id(), id);
            assert_eq!(Operation::from_id(id), Some(operation));
        }
        assert_eq!(Operation::from_id(b'?'), None);
    }
}
