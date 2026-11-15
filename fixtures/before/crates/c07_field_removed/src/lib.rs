/// An account with a name and a balance.
pub struct Account {
    /// The account name.
    pub name: &'static str,
    /// The balance in whole units.
    pub balance: u32,
}

impl Account {
    /// Opens an empty account.
    pub fn open(name: &'static str) -> Account {
        Account { name, balance: 0 }
    }

    /// Describes the account.
    pub fn describe(&self) -> String {
        format!("{}: {}", self.name, self.balance)
    }

    /// Returns the account name.
    pub fn name(&self) -> &'static str {
        self.name
    }
}
