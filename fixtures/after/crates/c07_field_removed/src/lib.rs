/// An account with a name.
pub struct Account {
    /// The account name.
    pub name: &'static str,
}

impl Account {
    /// Opens an empty account.
    pub fn open(name: &'static str) -> Account {
        Account { name }
    }

    /// Describes the account.
    pub fn describe(&self) -> String {
        format!("{}", self.name)
    }

    /// Returns the account name.
    pub fn name(&self) -> &'static str {
        self.name
    }
}
