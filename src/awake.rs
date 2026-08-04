use zbus::blocking::Connection;

const SERVICE: &str = "org.freedesktop.ScreenSaver";
const PATH: &str = "/org/freedesktop/ScreenSaver";
const APP: &str = "health-widget";
const REASON: &str = "автопилот работает в браузере";

pub struct Awake {
    conn: Option<Connection>,
    cookie: Option<u32>,
}

impl Awake {
    pub fn new() -> Self {
        Self {
            conn: None,
            cookie: None,
        }
    }

    pub fn set(&mut self, on: bool) {
        match (on, self.cookie) {
            (true, None) => self.inhibit(),
            (false, Some(cookie)) => self.uninhibit(cookie),
            _ => {}
        }
    }

    fn conn(&mut self) -> Option<&Connection> {
        if self.conn.is_none() {
            self.conn = Connection::session().ok();
        }
        self.conn.as_ref()
    }

    fn inhibit(&mut self) {
        let Some(conn) = self.conn() else {
            return;
        };
        let reply = conn.call_method(Some(SERVICE), PATH, Some(SERVICE), "Inhibit", &(APP, REASON));
        self.cookie = reply.ok().and_then(|r| r.body().deserialize::<u32>().ok());
    }

    fn uninhibit(&mut self, cookie: u32) {
        self.cookie = None;
        let Some(conn) = self.conn() else {
            return;
        };
        let _ = conn.call_method(Some(SERVICE), PATH, Some(SERVICE), "UnInhibit", &(cookie,));
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        if let Some(cookie) = self.cookie {
            self.uninhibit(cookie);
        }
    }
}
