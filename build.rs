//! Icône de l'exécutable (reprise de NZXT CAM), aussi chargée par la fenêtre et la zone de notification.

fn main() {
    println!("cargo:rerun-if-changed=assets/nzxt-lite.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    embed_resource::compile("assets/nzxt-lite.rc", embed_resource::NONE).manifest_optional().expect("ressources Windows");
}
