#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Ascending,
    Descending,
}

impl SortKey {
    pub fn as_str(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Size => "size",
            SortKey::Modified => "modified",
            SortKey::Kind => "kind",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "name" => Some(SortKey::Name),
            "size" => Some(SortKey::Size),
            "modified" => Some(SortKey::Modified),
            "kind" => Some(SortKey::Kind),
            _ => None,
        }
    }
}

impl Order {
    pub fn as_str(self) -> &'static str {
        match self {
            Order::Ascending => "ascending",
            Order::Descending => "descending",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ascending" | "asc" => Some(Order::Ascending),
            "descending" | "desc" => Some(Order::Descending),
            _ => None,
        }
    }

    pub fn reversed(self) -> Self {
        match self {
            Order::Ascending => Order::Descending,
            Order::Descending => Order::Ascending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_orders_round_trip() {
        for key in [
            SortKey::Name,
            SortKey::Size,
            SortKey::Modified,
            SortKey::Kind,
        ] {
            assert_eq!(SortKey::parse(key.as_str()), Some(key));
        }
        assert_eq!(Order::parse("desc"), Some(Order::Descending));
        assert_eq!(Order::Ascending.reversed(), Order::Descending);
        assert_eq!(SortKey::parse("nonsense"), None);
    }
}
