#[derive(Clone)]
pub enum InputField {
    Username,
    Password,
    Hostname,
    Port,
}

#[derive(Clone)]
pub struct ConnectionInput {
    pub username: String,
    pub password: String,
    pub hostname: String,
    pub port: String,
    pub current_field: InputField,
}

impl ConnectionInput {
    pub fn new() -> Self {
        Self {
            username: String::new(),
            password: String::new(),
            hostname: String::new(),
            port: String::new(),
            current_field: InputField::Username,
        }
    }
}