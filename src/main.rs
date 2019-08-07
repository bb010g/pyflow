use crate::dep_types::{Constraint, DepNode, Lock, LockPackage, Package, Req, Version};
use crate::util::abort;
use regex::Regex;
use serde::Deserialize;
use std::{
    collections::HashMap,
    env,
    error::Error,
    fmt, fs,
    io::{self, BufRead, BufReader},
    path::PathBuf,
    str::FromStr,
};
use structopt::StructOpt;
use termion::{color, style};

mod build;
mod commands;
mod dep_resolution;
mod dep_types;
mod edit_files;
mod install;
mod util;

#[derive(Copy, Clone, Debug)]
pub enum PackageType {
    Wheel,
    Source,
}

#[derive(Copy, Clone, Debug, PartialEq)]
/// Used to determine which version of a binary package to download. Assume 64-bit.
enum Os {
    Linux32,
    Linux,
    Windows32,
    Windows,
    Mac32,
    Mac,
    Any,
}

#[derive(StructOpt, Debug)]
#[structopt(name = "Pypackage", about = "Python packaging and publishing")]
struct Opt {
    #[structopt(subcommand)]
    subcmds: Option<SubCommand>,
    #[structopt(name = "custom_bin")]
    //    custom_bin: Vec<String>,
    custom_bin: Vec<String>,
}

///// eg `ipython`, `black` etc.
//#[derive(StructOpt, Debug)]
//struct CustomBin {
//    test: bool,
////    #[structopt(name = "name")]
////    name: String,
////    #[structopt(name = "args")]
////    args: Vec<String>,
//}

#[derive(StructOpt, Debug)]
enum SubCommand {
    /// Create a project folder with the basics
    #[structopt(name = "new")]
    New {
        #[structopt(name = "name")]
        name: String, // holds the project name.
    },

    /// Install packages from `pyproject.toml`, or ones specified
    #[structopt(
        name = "install",
        help = "
Install packages from `pyproject.toml`, `pypackage.lock`, or speficied ones. Example:

`pypackage install`: sync your installation with `pyproject.toml`, or `pypackage.lock` if it exists.
`pypackage install numpy scipy`: install `numpy` and `scipy`.
"
    )]
    Install {
        #[structopt(name = "packages")]
        packages: Vec<String>,
        #[structopt(short = "b", long = "binary")]
        bin: bool,
    },
    /// Uninstall all packages, or ones specified
    #[structopt(name = "uninstall")]
    Uninstall {
        #[structopt(name = "packages")]
        packages: Vec<String>,
    },
    /// Run python
    #[structopt(name = "python")]
    Python {
        #[structopt(name = "args")]
        args: Vec<String>,
    },
    /// Build the package, wrapping `setuptools`
    #[structopt(name = "package")]
    Package,
    /// Publish to `pypi`
    #[structopt(name = "publish")]
    Publish,
    /// Create a `pyproject.toml` from requirements.txt, pipfile etc, setup.py etc
    #[structopt(name = "init")]
    Init,
}

/// A config, parsed from pyproject.toml
#[derive(Clone, Debug, Default, Deserialize)]
// todo: Auto-desr some of these!
pub struct Config {
    py_version: Option<Version>,
    dependencies: Vec<Req>, // name, requirements.
    name: Option<String>,
    version: Option<Version>,
    author: Option<String>,
    author_email: Option<String>,
    description: Option<String>,
    classifiers: Vec<String>, // https://pypi.org/classifiers/
    keywords: Vec<String>,
    homepage: Option<String>,
    repo_url: Option<String>,
    package_url: Option<String>,
    readme_filename: Option<String>,
    license: Option<String>,
}

fn key_re(key: &str) -> Regex {
    Regex::new(&format!(r#"^{}\s*=\s*"(.*)"$"#, key)).unwrap()
}

impl Config {
    /// Pull config data from `pyproject.toml`
    fn from_file(filename: &str) -> Option<Self> {
        // We don't use the `toml` crate here because it doesn't appear flexible enough.
        let mut result = Config::default();
        let file = match fs::File::open(filename) {
            Ok(f) => f,
            Err(_) => return None,
        };

        let mut in_sect = false;
        let mut in_dep = false;

        let sect_re = Regex::new(r"\[.*\]").unwrap();

        for line in BufReader::new(file).lines() {
            if let Ok(l) = line {
                // todo replace this with something that clips off
                // todo post-# part of strings; not just ignores ones starting with #
                if l.starts_with('#') {
                    continue;
                }

                if &l == "[tool.pypackage]" {
                    in_sect = true;
                    in_dep = false;
                    continue;
                } else if &l == "[tool.pypackage.dependencies]" {
                    in_sect = false;
                    in_dep = true;
                    continue;
                } else if sect_re.is_match(&l) {
                    in_sect = false;
                    in_dep = false;
                    continue;
                }

                if in_sect {
                    // todo DRY
                    if let Some(n2) = key_re("name").captures(&l) {
                        if let Some(n) = n2.get(1) {
                            result.name = Some(n.as_str().to_string());
                        }
                    }
                    if let Some(n2) = key_re("description").captures(&l) {
                        if let Some(n) = n2.get(1) {
                            result.description = Some(n.as_str().to_string());
                        }
                    }
                    if let Some(n2) = key_re("version").captures(&l) {
                        if let Some(n) = n2.get(1) {
                            let n3 = n.as_str();
                            if !n3.is_empty() {
                                result.version = Some(Version::from_str(n3).unwrap());
                            }
                        }
                    }
                    if let Some(n2) = key_re("py_version").captures(&l) {
                        if let Some(n) = n2.get(1) {
                            let n3 = n.as_str();
                            if !n3.is_empty() {
                                result.py_version = Some(Version::from_str(n.as_str()).unwrap());
                            }
                        }
                    }
                } else if in_dep {
                    if !l.is_empty() {
                        result.dependencies.push(Req::from_str(&l, false).unwrap());
                    }
                }
            }
        }

        Some(result)
    }

    /// Create a new `pyproject.toml` file.
    fn write_file(&self, filename: &str) {
        let file = PathBuf::from(filename);
        if file.exists() {
            abort("`pyproject.toml` already exists")
        }

        // todo: Use a bufer instead of String?
        let mut result = String::new();

        result.push_str("[tool.pypackage]\n");
        if let Some(name) = &self.name {
            result.push_str(&("name = \"".to_owned() + name + "\"\n"));
        } else {
            // Give name, and a few other fields default values.
            result.push_str(&("name = \"\"".to_owned() + "\n"));
        }
        if let Some(py_v) = self.py_version {
            result.push_str(&("version = \"".to_owned() + &py_v.to_string() + "\"\n"));
        } else {
            result.push_str(&("version = \"\"".to_owned() + "\n"));
        }
        if let Some(vers) = self.version {
            result.push_str(&(vers.to_string() + "\n"));
        }
        if let Some(author) = &self.author {
            result.push_str(&(author.to_owned() + "\n"));
        }

        result.push_str("\n\n");
        result.push_str("[tool.pypackage.dependencies]\n");
        for dep in self.dependencies.iter() {
            result.push_str(&(dep.to_cfg_string() + "\n"));
        }

        match fs::write(file, result) {
            Ok(_) => println!("Created `pyproject.toml`"),
            Err(_) => abort("Problem writing `pyproject.toml`"),
        }
    }
}

/// Create a template directory for a python project.
pub(crate) fn new(name: &str) -> Result<(), Box<Error>> {
    if !PathBuf::from(name).exists() {
        fs::create_dir_all(&format!("{}/{}", name, name))?;
        fs::File::create(&format!("{}/{}/main.py", name, name))?;
        fs::File::create(&format!("{}/README.md", name))?;
        fs::File::create(&format!("{}/LICENSE", name))?;
        fs::File::create(&format!("{}/pyproject.toml", name))?;
        fs::File::create(&format!("{}/.gitignore", name))?;
    }

    let gitignore_init = r##"# General Python ignores

build/
dist/
__pycache__/
.ipynb_checkpoints/
*.pyc
*~
*/.mypy_cache/


# Project ignores
"##;

    let pyproject_init = &format!(
        r##"[tool.pypackage]
name = "{}"
py_version = "3.7"
version = "0.1.0"
description = ""
author = ""

pyackage_url = "https://test.pypi.org"
# pyackage_url = "https://pypi.org"

[tool.pypackage.dependencies]
"##,
        name
    );

    // todo: flesh readme out
    let readme_init = &format!("# {}", name);

    fs::write(&format!("{}/.gitignore", name), gitignore_init)?;
    fs::write(&format!("{}/pyproject.toml", name), pyproject_init)?;
    fs::write(&format!("{}/README.md", name), readme_init)?;

    Ok(())
}

/// Prompt which Python alias to use, if multiple are found.
fn prompt_alias(aliases: &[(String, Version)]) -> (String, Version) {
    // Todo: Overall, the API here is inelegant.
    println!("Found multiple Python aliases. Please enter the number associated with the one you'd like to use for this project:");
    for (i, (alias, version)) in aliases.iter().enumerate() {
        println!("{}: {} version: {}", i + 1, alias, version.to_string())
    }

    let mut mapping = HashMap::new();
    for (i, alias) in aliases.iter().enumerate() {
        mapping.insert(i + 1, alias);
    }

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .expect("Unable to read user input for version");

    let input = input
        .chars()
        .next()
        .expect("Problem reading input")
        .to_string();

    let (alias, version) = mapping
        .get(
            &input
                .parse::<usize>()
                .expect("Enter the number associated with the Python alias."),
        )
        .expect(
            "Can't find the Python alias associated with that number. Is it in the list above?",
        );
    (alias.to_string(), *version)
}

#[derive(Debug)]
pub struct AliasError {
    details: String,
}

impl Error for AliasError {
    fn description(&self) -> &str {
        &self.details
    }
}

impl fmt::Display for AliasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.details)
    }
}

/// Make an educated guess at the command needed to execute python the
/// current system.  An alternative approach is trying to find python
/// installations.
fn find_py_alias() -> Result<(String, Version), AliasError> {
    let possible_aliases = &[
        "python3.10",
        "python3.9",
        "python3.8",
        "python3.7",
        "python3.6",
        "python3.5",
        "python3.4",
        "python3.3",
        "python3.2",
        "python3.1",
        "python3",
        "python",
        "python2",
    ];

    let mut found_aliases = Vec::new();

    for alias in possible_aliases {
        // We use the --version command as a quick+effective way to determine if
        // this command is associated with Python.
        if let Some(v) = commands::find_py_version(alias) {
            found_aliases.push((alias.to_string(), v));
        }
    }

    match possible_aliases.len() {
        0 => Err(AliasError {
            details: "Can't find Python on the path.".into(),
        }),
        1 => Ok(found_aliases[0].clone()),
        _ => Ok(prompt_alias(&found_aliases)),
    }
}

/// Read dependency data from a lock file.
fn read_lock(filename: &str) -> Result<(Lock), Box<Error>> {
    let data = fs::read_to_string(filename)?;
    //    let t: Lock = toml::from_str(&data).unwrap();
    Ok(toml::from_str(&data)?)
}

/// Write dependency data to a lock file.
fn write_lock(filename: &str, data: &Lock) -> Result<(), Box<Error>> {
    let data = toml::to_string(data)?;
    fs::write(filename, data)?;
    Ok(())
}

/// Find the operating system from a wheel filename. This doesn't appear to be available
/// anywhere else on the Pypi Warehouse.
fn os_from_wheel_fname(filename: &str) -> Result<(Os), dep_types::DependencyError> {
    // Format is "name-version-pythonversion-?-os"
    let re = Regex::new(r"^.*-.*-.*-.*-(.*).whl$").unwrap();
    if let Some(caps) = re.captures(filename) {
        let parsed = caps.get(1).unwrap().as_str();

        let result = match parsed {
            "manylinux1_i686" => Os::Linux32,
            "manylinux1_x86_64" => Os::Linux,
            "win32" => Os::Windows32,
            "win_amd64" => Os::Windows,
            "any" => Os::Any,
            _ => {
                if parsed.contains("mac") {
                    Os::Mac
                } else {
                    abort(&format!("Unknown OS type in wheel filename: {}", parsed));
                    Os::Linux // todo dummy for match
                }
            }
        };
        return Ok(result);
    }

    Err(dep_types::DependencyError::new(
        "Problem parsing os from wheel name",
    ))
}

fn create_venv(cfg_v: Option<&Version>, pyypackage_dir: &PathBuf) -> Version {
    // We only use the alias for creating the virtual environment. After that,
    // we call our venv's executable directly.

    // todo perhaps move alias finding back into create_venv, or make a
    // todo create_venv_if_doesnt_exist fn.
    let (alias, py_ver_from_alias) = match find_py_alias() {
        Ok(a) => a,
        Err(_) => {
            abort("Unable to find a Python version on the path");
            ("".to_string(), Version::new_short(0, 0)) // Required for compiler
        }
    };

    let lib_path = pyypackage_dir.join(format!(
        "{}.{}/lib",
        py_ver_from_alias.major, py_ver_from_alias.minor
    ));
    if !lib_path.exists() {
        fs::create_dir_all(&lib_path).expect("Problem creating __pypackages__ directory");
    }

    if let Some(c_v) = cfg_v {
        // We don't expect the config version to specify a patch, but if it does, take it
        // into account.
        if c_v != &py_ver_from_alias {
            println!("{:?}, {:?}", c_v, &py_ver_from_alias);
            abort(&format!("The Python version you selected ({}) doesn't match the one specified in `pyprojecttoml` ({})",
                           py_ver_from_alias.to_string(), c_v.to_string())
            );
        }
    }

    println!("Setting up Python environment...");

    if commands::create_venv(&alias, &lib_path, ".venv").is_err() {
        util::abort("Problem creating virtual environment");
    }

    // Wait until the venv's created before continuing, or we'll get errors
    // when attempting to use it
    // todo: These won't work with Scripts ! - pass venv_path et cinstead
    let py_venv = lib_path.join("../.venv/bin/python");
    let pip_venv = lib_path.join("../.venv/bin/pip");
    util::wait_for_dirs(&[py_venv, pip_venv]).unwrap();

    py_ver_from_alias
}

/// Find teh packages installed, by browsing the lib folder.
fn find_installed(lib_path: &PathBuf) -> Vec<(String, Version)> {
    // todo: More functional?
    let mut package_folders = vec![];
    for entry in lib_path.read_dir().unwrap() {
        if let Ok(entry) = entry {
            if entry.file_type().unwrap().is_dir() {
                package_folders.push(entry.file_name())
            }
        }
    }

    let mut result = vec![];

    for folder in package_folders.iter() {
        let folder_name = folder.to_str().unwrap();
        let re = Regex::new(r"^(.*?)-(.*?)\.dist-info$").unwrap();
        let re_egg = Regex::new(r"^(.*?)-(.*?)\.egg-info$").unwrap();

        if let Some(caps) = re.captures(&folder_name) {
            let name = caps.get(1).unwrap().as_str();
            let vers = Version::from_str(caps.get(2).unwrap().as_str()).unwrap();
            result.push((name.to_owned(), vers));

        // todo dry
        } else if let Some(caps) = re_egg.captures(&folder_name) {
            let name = caps.get(1).unwrap().as_str();
            let vers = Version::from_str(caps.get(2).unwrap().as_str()).unwrap();
            result.push((name.to_owned(), vers));
        }
    }
    result
}

/// Uninstall and install packages to be in accordance with the lock.
fn sync_packages_with_lock(
    bin_path: &PathBuf,
    lib_path: &PathBuf,
    lock_packs: &Vec<LockPackage>,
    installed: &Vec<(String, Version)>,
) {
    // Uninstall packages no longer needed.
    for (name_ins, vers_ins) in installed.iter() {
        if !lock_packs
            .iter()
            .map(|lp| {
                (
                    lp.name.to_owned().to_lowercase(),
                    Version::from_str(&lp.version).unwrap(),
                )
            })
            .collect::<Vec<(String, Version)>>()
            .contains(&(name_ins.to_owned().to_lowercase(), *vers_ins))
            || name_ins.to_lowercase() == "twine"
            || name_ins.to_lowercase() == "setuptools"
            || name_ins.to_lowercase() == "setuptools"
        {}
    }

    for lock_pack in lock_packs {
        let p = Package::from_lock_pack(lock_pack);
        if installed
            .iter()
            // Set both names to lowercase to ensure case doesn't preclude a match.
            .map(|(p_name, p_vers)| (p_name.clone().to_lowercase(), *p_vers))
            .collect::<Vec<(String, Version)>>()
            .contains(&(p.name.clone().to_lowercase(), p.version))
        {
            continue; // Already installed.
        }

        // path_to_info is the path to the metadatafolder, ie dist-info (or egg-info for older packages).
        // todo: egg-info
        // when making the path, use the LockPackage vice p, since its version's already serialized.
        //        let path_to_dep = lib_path.join(&lock_pack.name);
        //        let path_to_info = lib_path.join(format!(
        //            "{}-{}.dist-info",
        //            lock_pack.name, lock_pack.version
        //        ));

        //        if commands::install(&bin_path, &[p], false, false).is_err() {
        //            abort("Problem installing packages");
        //        }
        //        download_and_install_package(p.file_url, p.filename, p.hash_, lib_path, false);
    }
}

/// Install/uninstall deps as required from the passed list, and re-write the lock file.
fn sync_deps(
    lock_filename: &str,
    bin_path: &PathBuf,
    lib_path: &PathBuf,
    reqs: &mut Vec<Req>,
    installed: &Vec<(String, Version)>,
    python_vers: &Version,
    os: Os,
) {
    println!("Resolving dependencies...");

    // Recursively add sub-dependencies.
    let mut tree = DepNode {
        // dummy parent
        name: String::from("root"),
        version: Version::new(0, 0, 0),
        reqs: reqs.clone(), // todo clone?
        dependencies: vec![],
        constraints_for_this: vec![],
    };

    let resolved = match dep_resolution::resolve(&mut tree) {
        Ok(r) => r,
        Err(_) => {
            abort("Problem resolving dependencies");
            vec![] // todo find proper way to equlaize mathc arms.
        }
    };

    // Resolve is made from non-nested deps, with their subdeps stripped: It's flattened.
    for dep in resolved.iter() {
        // Move on if we've already installed this specific package/version
        let mut already_installed = false;
        for (inst_name, inst_ver) in installed.iter() {
            if *inst_name.to_lowercase() == dep.name.to_lowercase() && *inst_ver == dep.version {
                already_installed = true;
            }
        }
        if already_installed {
            continue;
        }

        let data = dep_resolution::get_warehouse_release(&dep.name, &dep.version)
            .expect("Problem getting warehouse data");

        let mut compatible_releases = vec![];
        // Store source releases as a fallback, for if no wheels are found.
        let mut source_releases = vec![];

        for rel in data.iter() {
            let mut compatible = true;
            match rel.packagetype.as_ref() {
                "bdist_wheel" => {
                    if let Some(py_ver) = &rel.requires_python {
                        // If a version constraint exists, make sure it's compatible.
                        let py_req = Constraint::from_str(&py_ver)
                            .expect("Problem parsing constraint from requires_python");

                        if !py_req.is_compatible(&python_vers) {
                            compatible = false;
                        }
                    }

                    let wheel_os = os_from_wheel_fname(&rel.filename)
                        .expect("Problem getting os from wheel name");
                    if wheel_os != os && wheel_os != Os::Any {
                        compatible = false;
                    }

                    // Packages that use C code(eg numpy) may fail to load C extensions if installing
                    // for the wrong version of python (eg  cp35 when python 3.7 is installed), even
                    // if `requires_python` doesn't indicate an incompatibility. Check `python_version`.
                    match Version::from_cp_str(&rel.python_version) {
                        Ok(req_v) => {
                            if req_v != *python_vers
                                // todo: Awk place for this logic.
                                && rel.python_version != "py2.py3"
                                && rel.python_version != "py3"
                            {
                                compatible = false;
                            }
                        }
                        Err(e) => {
                            (println!(
                                "Unable to match python version from python_version: {}",
                                &rel.python_version
                            ))
                        } // todo
                    }

                    if compatible {
                        compatible_releases.push(rel.clone());
                    }
                }
                "sdist" => source_releases.push(rel.clone()),
                _ => abort(&format!(
                    "Found surprising package type: {}",
                    rel.packagetype
                )),
            }
        }

        let best_release;
        let package_type;
        // todo: Sort further / try to match exact python_version if able.
        if compatible_releases.is_empty() {
            if source_releases.is_empty() {
                abort(&format!(
                    "Unable to find a compatible release for {}: {}",
                    dep.name,
                    dep.version.to_string()
                ));
                best_release = &compatible_releases[0]; // todo temp
                package_type = PackageType::Wheel // todo temp to satisfy match
            } else {
                best_release = &source_releases[0];
                package_type = PackageType::Source;
            }
        } else {
            best_release = &compatible_releases[0];
            package_type = PackageType::Wheel;
        }

        println!(
            "Downloading and installing {} = \"{}\"",
            &dep.name,
            &dep.version.to_string()
        );

        if install::download_and_install_package(
            &best_release.url,
            &best_release.filename,
            &best_release.md5_digest,
            lib_path,
            bin_path,
            false,
            package_type,
        )
        .is_err()
        {
            abort("Problem downloading packages");
        }
    }

    for (inst_name, inst_vers) in installed.iter() {
        let mut required = false;
        for dep in resolved.iter() {
            if dep.name.to_lowercase() == *inst_name.to_lowercase() && dep.version == *inst_vers {
                required = true;
            }
        }
        if !required {
            install::uninstall(inst_name, inst_vers, lib_path)
        }
    }

    //    let lock_metadata = resolved.iter().map(|dep|
    //        // todo: Probably incorporate hash etc info in the depNode.
    //        format!("\"checksum {} {} ({})\" = \"{}\"", &dep.name, &dep.version.to_string(), "", "placeholder")
    //    )
    //        .collect();

    let lock_packs = resolved
        .into_iter()
        .map(|dep| LockPackage {
            name: dep.name.clone(),
            version: dep.version.to_string(),
            source: Some(format!(
                "pypi+https://pypi.org/pypi/{}/{}/json",
                dep.name,
                dep.version.to_string()
            )), // todo
            dependencies: None, // todo!
        })
        .collect();

    let new_lock = Lock {
        //        metadata: Some(lock_metadata),
        metadata: None, // todo: Problem with toml conversion.
        package: Some(lock_packs),
    };

    if write_lock(lock_filename, &new_lock).is_err() {
        abort("Problem writing lock file");
    }
}

fn main() {
    // todo perhaps much of this setup code should only be in certain match branches.
    let cfg_filename = "pyproject.toml";
    let lock_filename = "pypackage.lock";

    let mut cfg = Config::from_file(cfg_filename).unwrap_or_default();

    let opt = Opt::from_args();
    let subcmd = match opt.subcmds {
        Some(sc) => sc,
        None => {
            abort("No command entered. For a list of what you can do, run `pyproject --help`.");
            SubCommand::Init {} // Dummy to satisfy the compiler.
        }
    };

    // New doesn't execute any other logic. Init must execute befor the rest of the logic,
    // since it sets up a new (or modified) `pyproject.toml`. The rest of the commands rely
    // on the virtualenv and `pyproject.toml`, so make sure those are set up before processing them.
    match subcmd {
        SubCommand::New { name } => {
            new(&name).expect("Problem creating project");
            println!("Created a new Python project named {}", name);
            return;
        }
        SubCommand::Init {} => {
            edit_files::parse_req_dot_text(&mut cfg);
            edit_files::parse_pipfile(&mut cfg);
            edit_files::parse_poetry(&mut cfg);
            edit_files::update_pyproject(&cfg);

            cfg.write_file(cfg_filename);
        }
        _ => (),
    }

    let pypackage_dir = env::current_dir()
        .expect("Can't find current path")
        .join("__pypackages__");

    let py_version_cfg = cfg.py_version;

    // Check for environments. Create one if none exist. Set `vers_path`.
    let mut vers_path = PathBuf::new();
    let mut py_vers = Version::new(0, 0, 0);

    match py_version_cfg {
        // The version's explicitly specified; check if an environment for that version
        // exists. If not, create one, and make sure it's the right version.
        Some(cfg_v) => {
            // The version's specified in the config. Ensure a virtualenv for this
            // is setup.  // todo: Confirm using --version on the python bin, instead of relying on folder name.

            // Don't include version patch in the directory name, per PEP 582.
            vers_path = pypackage_dir.join(&format!("{}.{}", cfg_v.major, cfg_v.minor));
            py_vers = Version::new_short(cfg_v.major, cfg_v.minor);

            if !util::venv_exists(&vers_path.join(".venv")) {
                let created_vers = create_venv(Some(&cfg_v), &pypackage_dir);
            }
        }
        // The version's not specified in the config; Search for existing environments, and create
        // one if we can't find any.
        None => {
            // Note that we rely on the proper folder name, vice inspecting the binary.
            // ie: could also check `bin/python --version`.
            let venv_versions_found: Vec<Version> = util::possible_py_versions()
                .into_iter()
                .filter(|v| {
                    util::venv_exists(
                        &pypackage_dir.join(&format!("{}.{}/.venv", v.major, v.minor)),
                    )
                })
                .collect();

            match venv_versions_found.len() {
                0 => {
                    let created_vers = create_venv(None, &pypackage_dir);
                    vers_path = pypackage_dir
                        .join(&format!("{}.{}", created_vers.major, created_vers.minor));
                    py_vers = Version::new_short(created_vers.major, created_vers.minor);
                }
                1 => {
                    vers_path = pypackage_dir.join(&format!(
                        "{}.{}",
                        venv_versions_found[0].major, venv_versions_found[0].minor
                    ));
                    py_vers = Version::new_short(
                        venv_versions_found[0].major,
                        venv_versions_found[0].minor,
                    );
                }
                _ => abort(
                    "Multiple Python environments found
                for this project; specify the desired one in `pyproject.toml`. Example:
[tool.pyproject]
py_version = \"3.7\"",
                ),
            }
        }
    };

    let lib_path = vers_path.join("lib");
    let (bin_path, lib_bin_path) = util::find_bin_path(&vers_path);

    let lock = match read_lock(lock_filename) {
        Ok(l) => {
            println!("Found lockfile");
            l
        }
        Err(_) => Lock::default(),
    };

    let args = opt.custom_bin;
    if !args.is_empty() {
        // todo better handling, eg abort
        let name = args.get(0).expect("Missing first arg").clone();
        let args: Vec<String> = args.into_iter().skip(1).collect();
        if commands::run_bin(&bin_path, &lib_path, &name, &args).is_err() {
            abort(&format!(
                "Problem running the binary script {}. Is it installed? \
                 Try running `pypackage install {} -b`",
                name, name
            ));
        }

        return;
    }

    match subcmd {
        // Add pacakge names to `pyproject.toml` if needed. Then sync installed packages
        // and `pyproject.lock` with the `pyproject.toml`.
        SubCommand::Install { packages, bin } => {
            let mut added_deps: Vec<Req> = packages
                .into_iter()
                .map(|p| Req::from_str(&p, false).unwrap())
                .collect();

            let installed = find_installed(&lib_path);

            // todo: Compare to existing listed lock_packs and merge appropriately.
            edit_files::add_dependencies(cfg_filename, &added_deps);

            let mut reqs = cfg.dependencies.clone();

            // todo excessive nesting
            if let Some(lock_packs) = lock.package {
                for req in reqs.iter_mut() {
                    for lock_pack in lock_packs.iter() {
                        let lock_vers = Version::from_str(&lock_pack.version).unwrap();
                        if lock_pack.name == req.name {
                            let mut compatible = true;
                            for constraint in req.constraints.iter() {
                                if !constraint.is_compatible(&lock_vers) {
                                    compatible = false;
                                    break;
                                }
                            }
                            if compatible {
                                // Fix the constraint to the lock if compatible.
                                // todo printline temp
                                println!("Locking constraint: {} -> {}", &req.to_cfg_string(), lock_vers.to_string());
                                req.constraints = vec![Constraint::new(
                                    dep_types::ReqType::Exact,
                                    lock_vers.major,
                                    lock_vers.minor,
                                    lock_vers.patch,
                                )];
                            }
                        }
                    }
                }
            }

            reqs.append(&mut added_deps);

            #[cfg(target_os = "windows")]
            let os = Os::Windows;
            #[cfg(target_os = "linux")]
            let os = Os::Linux;
            #[cfg(target_os = "macos")]
            let os = Os::Mac;

            // todo: Determine os.
            sync_deps(
                lock_filename,
                &bin_path,
                &lib_path,
                &mut reqs,
                &installed,
                &py_vers,
                os,
            );
            println!("Installation complete")
        }
        SubCommand::Uninstall { packages } => {
            // todo: DRY with ::Install
            let removed_deps: Vec<Req> = packages
                .into_iter()
                .map(|p| Req::from_str(&p, false).unwrap())
                .collect();

            edit_files::remove_dependencies(cfg_filename, &removed_deps);

            let installed = find_installed(&lib_path);
            sync_deps(
                lock_filename,
                &bin_path,
                &lib_path,
                &mut cfg.dependencies,
                &installed,
                &py_vers,
                Os::Linux,
            )
        }

        SubCommand::Python { args } => {
            if commands::run_python(&bin_path, &lib_path, &args).is_err() {
                abort("Problem running Python");
            }
        }
        SubCommand::Package {} => build::build(&bin_path, &lib_path, &cfg),
        SubCommand::Publish {} => build::publish(&bin_path, &cfg),

        // We already handled init
        SubCommand::Init {} => (),
        SubCommand::New { name } => (),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

}
