//! Incrusta el ícono (`app.rc` → `assets/nocho.ico`) en el .exe: se ve en el Explorador, el
//! Administrador de tareas y en "Aplicaciones de arranque". También el manifiesto
//! (`app.manifest`), que activa el estilo moderno de los menús. Usa el compilador de recursos del
//! Windows SDK (rc.exe), que viene con las Build Tools de Visual Studio.

fn main() {
    println!("cargo:rerun-if-changed=assets/nocho.ico");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=app.rc");
    embed_resource::compile("app.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("no se pudo compilar app.rc (¿falta el Windows SDK?)");
}
