//! Registro simples: por padrão vai para o stderr; a interface gráfica troca o destino
//! para mostrar as mensagens na janela.
use std::sync::RwLock;

type Sink = Box<dyn Fn(&str) + Send + Sync>;
static SINK: RwLock<Option<Sink>> = RwLock::new(None);

pub fn set_sink(f: impl Fn(&str) + Send + Sync + 'static) {
    *SINK.write().unwrap() = Some(Box::new(f));
}

pub fn write(msg: &str) {
    match &*SINK.read().unwrap() {
        Some(f) => f(msg),
        None => eprintln!("{msg}"),
    }
}

#[macro_export]
macro_rules! logln {
    ($($t:tt)*) => { $crate::log::write(&format!($($t)*)) };
}
