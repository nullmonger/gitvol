use serde::Serialize;
use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};
use tokio::fs;

use crate::{
    domains::{repo::RawRepo, volume::Status as VolumeStatus},
    driver::{Driver, ItemVolume, VolumeInfo},
    services::{
        git::{Error as GitError, Git},
        volumes::{Error as VolumesError, Volumes},
    },
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Volumes(#[from] VolumesError),

    #[error(transparent)]
    Git(#[from] GitError),

    #[error("Failed deletion of directory {path} for {operation}. {kind:?}")]
    RemoveDir {
        path: PathBuf,
        operation: String,
        kind: ErrorKind,
    },
}

#[cfg_attr(test, derive(Debug, PartialEq, Clone))]
#[derive(Serialize)]
pub struct Status {
    pub status: VolumeStatus,
}

impl From<VolumeStatus> for Status {
    fn from(status: VolumeStatus) -> Self {
        Self { status }
    }
}

#[derive(Clone)]
pub struct Plugin {
    base_path: PathBuf,
    volumes: Volumes,
    git: Git,
}

impl Plugin {
    pub fn new(base_path: &Path, git: Git) -> Self {
        Self {
            base_path: base_path.to_path_buf(),
            volumes: Volumes::new(),
            git,
        }
    }
}

#[async_trait::async_trait]
impl Driver for Plugin {
    type Error = Error;
    type Status = Status;
    type Opts = RawRepo;

    async fn path(&self, name: &str) -> Result<Option<PathBuf>, Self::Error> {
        let Some(volume) = self.volumes.read(name).await else {
            eprintln!("WARN: Volume named {} not found", name);
            return Ok(None);
        };

        Ok(volume.path.clone())
    }

    async fn get(&self, name: &str) -> Result<VolumeInfo<Self::Status>, Self::Error> {
        let volume = self.volumes.try_read(name).await?;
        Ok(VolumeInfo {
            mountpoint: volume.path.clone(),
            status: Status {
                status: volume.status.clone(),
            },
        })
    }

    async fn list(&self) -> Result<Vec<ItemVolume>, Self::Error> {
        let list = self.volumes.read_all().await;
        Ok(list
            .iter()
            .map(|v| ItemVolume {
                name: v.name.clone(),
                mountpoint: v.path.clone(),
            })
            .collect())
    }

    async fn create(&self, name: &str, opts: Option<Self::Opts>) -> Result<(), Self::Error> {
        self.volumes.create(name, opts).await?;
        Ok(())
    }

    async fn remove(&self, name: &str) -> Result<(), Self::Error> {
        let Some(volume) = self.volumes.remove(name).await else {
            eprintln!("WARN: Volume named {} not found", name);
            return Ok(());
        };

        remove_dir_if_exists(volume.path.clone()).await?;

        Ok(())
    }
    async fn mount(&self, name: &str, id: &str) -> Result<PathBuf, Self::Error> {
        let mut volume = self.volumes.try_write(name).await?;

        if let Some(path) = volume.path.clone() {
            println!("Repository {} already cloned.", name);
            if volume.repo.refetch {
                println!("Attempting to refetch repository {} for id {}.", name, id);
                self.git.refetch(&path).await?;
            }
            volume.containers.insert(id.to_string());
            return Ok(path);
        }

        let path = volume.create_path_from(&self.base_path);
        if path.exists() {
            println!("Repository directory {:?} already exists. Remooving", &path);
            fs::remove_dir_all(&path)
                .await
                .map_err(|e| Error::RemoveDir {
                    path: path.clone(),
                    operation: "exists repository dir".to_string(),
                    kind: e.kind(),
                })?;
        }
        self.git.clone(&path, &volume.repo).await?;

        volume.containers.insert(id.to_string());
        volume.status = VolumeStatus::Clonned;

        println!("Volume {} mounted successfully.", name);
        Ok(path)
    }

    async fn unmount(&self, name: &str, id: &str) -> Result<(), Self::Error> {
        let Some(mut volume) = self.volumes.write(name).await else {
            eprintln!("WARN: Volume named {} not found", name);
            return Ok(());
        };

        volume.containers.remove(id);

        if !volume.containers.is_empty() {
            println!(
                "Volume {} still in use by containers. container_count={}",
                name,
                volume.containers.len(),
            );
            return Ok(());
        }

        volume.status = VolumeStatus::Cleared;
        remove_dir_if_exists(volume.path.clone()).await?;
        volume.path = None;

        println!("Volume {} unmounted successfully.", name);
        Ok(())
    }
}

async fn remove_dir_if_exists(path: Option<PathBuf>) -> Result<(), Error> {
    if let Some(path) = path
        && path.exists()
    {
        println!("Attempting to remove directory {:?}", &path);
        fs::remove_dir_all(&path)
            .await
            .map_err(|e| Error::RemoveDir {
                path: path.clone(),
                operation: "remove dir if exists".to_string(),
                kind: e.kind(),
            })?;
    }

    Ok(())
}

#[cfg(test)]
mod test_mocks {
    use super::*;
    use crate::services::git::test_mocks::TestRepo;
    use std::ops::Deref;
    use tempfile::{Builder as TempBuilder, TempDir};

    pub const VOLUME_NAME: &str = "test_volume";

    pub struct TempPlugin {
        plugin: Plugin,
        temp: TempDir,
    }

    impl Deref for TempPlugin {
        type Target = Plugin;

        fn deref(&self) -> &Self::Target {
            &self.plugin
        }
    }

    impl Plugin {
        pub async fn stub() -> Self {
            Self::new(&std::env::temp_dir(), Git::init().await.unwrap())
        }

        pub async fn temp() -> TempPlugin {
            let temp = TempBuilder::new().prefix("temp-gitvol-").tempdir().unwrap();
            let plugin = Self::new(temp.path(), Git::init().await.unwrap());
            TempPlugin { plugin, temp }
        }

        pub async fn with_volume(self, volume_name: &str, raw_repo: RawRepo) -> Self {
            self.create(volume_name, Some(raw_repo)).await.unwrap();
            self
        }

        pub async fn with_stub_volume(self) -> Self {
            self.with_volume(VOLUME_NAME, RawRepo::stub()).await
        }

        pub async fn test_is_empty_list(&self) -> &Self {
            let list = self.list().await.unwrap();
            assert_eq!(list.len(), 0);
            self
        }

        pub async fn test_in_list(&self, input_list: Vec<ItemVolume>) -> &Self {
            let list = self.list().await.unwrap();

            assert_eq!(list.len(), input_list.len());

            for item in input_list {
                let list_item = list.iter().find(|i| i.name == item.name);
                assert!(
                    list_item.is_some(),
                    "The volume named {} was not found in the list.",
                    item.name
                );

                let mountpoint = list_item.and_then(|i| i.mountpoint.clone());
                assert_eq!(item.mountpoint, mountpoint);
            }

            self
        }

        pub async fn test_in_list_by_names(&self, input_list: Vec<&str>) -> &Self {
            self.test_in_list(
                input_list
                    .into_iter()
                    .map(|name| ItemVolume {
                        name: name.to_string(),
                        mountpoint: None,
                    })
                    .collect(),
            )
            .await
        }

        pub async fn test_path_is(&self, volume_name: &str, path: Option<PathBuf>) -> &Self {
            let path_result = self.path(volume_name).await.unwrap();
            assert_eq!(path_result, path);
            self
        }

        pub async fn test_stub_path_is(&self, path: Option<PathBuf>) -> &Self {
            self.test_path_is(VOLUME_NAME, path).await
        }

        pub async fn test_get_volume(&self, volume_name: &str, info: VolumeInfo<Status>) -> &Self {
            let volume = self.get(volume_name).await.unwrap();
            assert_eq!(volume, info);
            self
        }

        pub async fn test_get_stub_volume(&self, info: VolumeInfo<Status>) -> &Self {
            self.test_get_volume(VOLUME_NAME, info).await
        }
    }

    impl TempPlugin {
        pub async fn with_temp_volume(self, volume_name: &str, raw_repo: RawRepo) -> Self {
            let plugin = self.plugin.with_volume(volume_name, raw_repo).await;

            Self {
                plugin,
                temp: self.temp,
            }
        }

        pub async fn with_stub_test_repo(self) -> (TestRepo, Self) {
            let test_repo = TestRepo::new();
            let plugin = self
                .with_temp_volume(VOLUME_NAME, test_repo.create_raw_repo(None, None, None))
                .await;
            (test_repo, plugin)
        }
    }

    impl TestRepo {
        pub fn create_raw_repo(
            &self,
            branch: Option<String>,
            tag: Option<String>,
            refetch: Option<String>,
        ) -> RawRepo {
            RawRepo {
                url: Some(self.path().as_os_str().to_str().unwrap().to_string()),
                branch,
                tag,
                refetch,
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::test_mocks::*;
    use super::*;
    use rstest::rstest;
    use std::ops::Deref;

    use crate::services::git::test_mocks::TestRepo;

    #[tokio::test]
    async fn list_empty_initial() {
        Plugin::stub().await.test_is_empty_list().await;
    }

    #[tokio::test]
    async fn path_nonexistent_returns_none() {
        Plugin::stub().await.test_stub_path_is(None).await;
    }

    #[tokio::test]
    async fn get_nonexistent_returns_error() {
        let plugin = Plugin::stub().await;

        let result = plugin.get(VOLUME_NAME).await;
        assert!(
            result.is_err(),
            "Retrieving a non-existent volume should result in an error."
        );

        let error = result.unwrap_err();
        assert!(matches!(error, Error::Volumes(VolumesError::NonExists(_))));
    }

    #[rstest]
    #[case(RawRepo::stub())]
    #[case(RawRepo { branch: Some("some_branch".into()), ..RawRepo::stub() })]
    #[case(RawRepo { tag: Some("som-tag".into()), ..RawRepo::stub() })]
    #[case(RawRepo { refetch: Some("true".into()), ..RawRepo::stub() })]
    #[tokio::test]
    async fn create_success_new_volume(#[case] raw_repo: RawRepo) {
        Plugin::stub()
            .await
            .with_volume(VOLUME_NAME, raw_repo)
            .await
            .test_in_list_by_names(vec![VOLUME_NAME])
            .await
            .test_stub_path_is(None)
            .await
            .test_get_stub_volume(VolumeInfo {
                status: VolumeStatus::Created.into(),
                mountpoint: None,
            })
            .await;
    }

    #[tokio::test]
    async fn create_duplicate_name_error() {
        let plugin = Plugin::stub().await.with_stub_volume().await;

        let second_creating = plugin.create(VOLUME_NAME, Some(RawRepo::stub())).await;
        assert!(
            second_creating.is_err(),
            "Recreating the volume (with the same name) should result in an error."
        );

        let error = second_creating.unwrap_err();
        assert!(matches!(
            error,
            Error::Volumes(VolumesError::AlreadyExists(_))
        ));

        plugin.test_in_list_by_names(vec![VOLUME_NAME]).await;
    }

    #[rstest]
    #[case("", Some(RawRepo::stub()))]
    #[case("     ", Some(RawRepo::stub()))]
    #[case(VOLUME_NAME, None)]
    #[case(VOLUME_NAME, Some(RawRepo::default()))]
    #[case(VOLUME_NAME, Some(RawRepo { branch: Some("some_branch".into()), tag: Some("some_tag".into()), ..RawRepo::stub()}))]
    #[case(VOLUME_NAME, Some(RawRepo { url: Some("ftp://host/path-to-git-repo".into()), ..RawRepo::default()}))]
    #[tokio::test]
    async fn create_invalid_params_error(
        #[case] volume_name: &str,
        #[case] raw_repo: Option<RawRepo>,
    ) {
        let plugin = Plugin::stub().await;

        let result = plugin.create(volume_name, raw_repo.clone()).await;
        assert!(
            result.is_err(),
            "Creating a volume with incorrect parameters should result in an error. name={:?}; options={:?}",
            volume_name,
            raw_repo
        );

        let error = result.unwrap_err();
        assert!(matches!(error, Error::Volumes(_)));
        plugin.test_is_empty_list().await;
    }

    #[tokio::test]
    async fn list_multiple_volumes() {
        Plugin::stub()
            .await
            .with_stub_volume()
            .await
            .with_volume("other_volume", RawRepo::stub())
            .await
            .test_in_list_by_names(vec![VOLUME_NAME, "other_volume"])
            .await;
    }

    #[tokio::test]
    async fn path_after_mount_returns_some() {
        let (_g, plugin) = Plugin::temp().await.with_stub_test_repo().await;

        let mountpoint = plugin.mount(VOLUME_NAME, "id-123").await.unwrap();

        plugin.test_stub_path_is(Some(mountpoint)).await;
    }

    #[tokio::test]
    async fn get_created_unmounted_status() {
        let (_g, plugin) = Plugin::temp().await.with_stub_test_repo().await;

        let created = plugin.get(VOLUME_NAME).await.unwrap();
        assert_eq!(
            created.status,
            Status {
                status: VolumeStatus::Created
            }
        );

        plugin.mount(VOLUME_NAME, "id-123").await.unwrap();
        plugin.unmount(VOLUME_NAME, "id-123").await.unwrap();

        let cleared = plugin.get(VOLUME_NAME).await.unwrap();
        assert_eq!(
            cleared.status,
            Status {
                status: VolumeStatus::Cleared
            }
        );
    }

    #[tokio::test]
    async fn get_after_mount_status_clonned() {
        let (_g, plugin) = Plugin::temp().await.with_stub_test_repo().await;

        let mountpoint = plugin.mount(VOLUME_NAME, "id-123").await.unwrap();

        assert!(mountpoint.exists());
        plugin
            .test_get_stub_volume(VolumeInfo {
                mountpoint: Some(mountpoint),
                status: Status {
                    status: VolumeStatus::Clonned,
                },
            })
            .await;
    }

    #[tokio::test]
    async fn remove_nonexistent_by_empty_ok() {
        let plugin = Plugin::stub().await;
        let result = plugin.remove("other_volume").await;
        assert!(result.is_ok());

        plugin.test_is_empty_list().await;
    }

    #[tokio::test]
    async fn remove_nonexistent_with_other_volumes_ok() {
        let plugin = Plugin::stub().await.with_stub_volume().await;

        let result = plugin.remove("other_volume").await;
        assert!(result.is_ok());

        plugin.test_in_list_by_names(vec![VOLUME_NAME]).await;
    }

    #[tokio::test]
    async fn remove_existing_unmounted_ok() {
        let plugin = Plugin::stub().await.with_stub_volume().await;

        let result = plugin.remove(VOLUME_NAME).await;
        assert!(result.is_ok());

        plugin
            .test_is_empty_list()
            .await
            .test_stub_path_is(None)
            .await;
    }

    #[tokio::test]
    async fn remove_existing_mounted_ok() {
        let (_g, plugin) = Plugin::temp().await.with_stub_test_repo().await;

        let mountpoint = plugin.mount(VOLUME_NAME, "id").await.unwrap();
        let result = plugin.remove(VOLUME_NAME).await;
        assert!(result.is_ok());

        plugin.test_is_empty_list().await;
        assert!(!mountpoint.exists());
    }

    #[tokio::test]
    async fn mount_first_time_clones_repo() {
        let (test_repo, plugin) = Plugin::temp().await.with_stub_test_repo().await;

        let mountpoint = plugin.mount(VOLUME_NAME, "id").await.unwrap();

        TestRepo::test_is_not_git(&mountpoint);
        test_repo.test_is_default_branch(&mountpoint);
    }

    #[tokio::test]
    async fn mount_when_already_mounted_no_clone() {
        let (_g, plugin) = Plugin::temp().await.with_stub_test_repo().await;

        let first_mountpoint = plugin.mount(VOLUME_NAME, "id-1").await.unwrap();
        let second_mountpoint = plugin.mount(VOLUME_NAME, "id-2").await.unwrap();

        assert_eq!(first_mountpoint, second_mountpoint);
    }

    #[tokio::test]
    async fn mount_with_branch() {
        let test_repo = TestRepo::new().with_branch("develop");
        let plugin = Plugin::temp()
            .await
            .with_temp_volume(
                VOLUME_NAME,
                test_repo.create_raw_repo(Some("develop".into()), None, None),
            )
            .await;

        let mountpoint = plugin.mount(VOLUME_NAME, "id").await.unwrap();
        TestRepo::test_is_branch(&mountpoint, "develop");
    }

    #[tokio::test]
    async fn mount_with_tag() {
        let test_repo = TestRepo::new().with_tag("v1");
        let plugin = Plugin::temp()
            .await
            .with_temp_volume(
                VOLUME_NAME,
                test_repo.create_raw_repo(None, Some("v1".into()), None),
            )
            .await;

        let mountpoint = plugin.mount(VOLUME_NAME, "id").await.unwrap();
        TestRepo::test_is_tag(&mountpoint, "v1");
    }

    #[tokio::test]
    async fn mount_with_refetch() {
        let branch_name = "some_branch";
        let test_repo = TestRepo::new().with_branch(branch_name);
        let plugin = Plugin::temp()
            .await
            .with_temp_volume(
                VOLUME_NAME,
                test_repo.create_raw_repo(Some(branch_name.into()), None, Some("true".into())),
            )
            .await;

        let mountpoint = plugin.mount(VOLUME_NAME, "id-1").await.unwrap();
        TestRepo::test_is_git(&mountpoint);
        TestRepo::test_is_branch(&mountpoint, branch_name);

        test_repo.change(branch_name, "changed value");

        plugin.mount(VOLUME_NAME, "id-2").await.unwrap();
        TestRepo::test_is_changed(&mountpoint, branch_name, "changed value");
    }

    #[tokio::test]
    async fn mount_clone_failure_on_bad_url() {
        let plugin = Plugin::stub().await.with_volume(
            VOLUME_NAME,
            RawRepo {
                url: Some("http://host/path-to-git-repo".into()),
                ..Default::default()
            },
        );

        let result = plugin.await.mount(VOLUME_NAME, "id").await;
        assert!(
            result.is_err(),
            " Mounting a non-existent repository should result in an error."
        );
    }

    #[tokio::test]
    async fn unmount_nonexistent_ok() {
        let plugin = Plugin::stub().await;

        let result = plugin.unmount("name", "id").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn unmount_with_multiple_containers_keeps_dir() {
        let (_g, plugin) = Plugin::temp().await.with_stub_test_repo().await;

        let mountpoint = plugin.mount(VOLUME_NAME, "id-1").await.unwrap();
        plugin.mount(VOLUME_NAME, "id-2").await.unwrap();

        plugin.unmount(VOLUME_NAME, "id-1").await.unwrap();
        plugin
            .test_in_list(vec![ItemVolume {
                name: VOLUME_NAME.into(),
                mountpoint: Some(mountpoint.clone()),
            }])
            .await
            .test_get_stub_volume(VolumeInfo {
                mountpoint: Some(mountpoint.clone()),
                status: VolumeStatus::Clonned.into(),
            })
            .await;
        assert!(mountpoint.exists());
    }

    #[tokio::test]
    async fn unmount_last_container_removes_dir_and_clears() {
        let (_g, plugin) = Plugin::temp().await.with_stub_test_repo().await;

        let mountpoint = plugin.mount(VOLUME_NAME, "id-1").await.unwrap();
        plugin.mount(VOLUME_NAME, "id-2").await.unwrap();

        plugin.unmount(VOLUME_NAME, "id-1").await.unwrap();
        plugin.unmount(VOLUME_NAME, "id-2").await.unwrap();

        plugin
            .test_in_list(vec![ItemVolume {
                name: VOLUME_NAME.into(),
                mountpoint: None,
            }])
            .await
            .test_get_stub_volume(VolumeInfo {
                mountpoint: None,
                status: VolumeStatus::Cleared.into(),
            })
            .await;

        assert!(!mountpoint.exists());
    }

    #[tokio::test]
    async fn unmount_unknown_container_id_no_panic() {
        let (_g, plugin) = Plugin::temp().await.with_stub_test_repo().await;

        let mountpoint = plugin.mount(VOLUME_NAME, "id-1").await.unwrap();
        let result = plugin.unmount(VOLUME_NAME, "ghost-id").await;
        assert!(result.is_ok());

        assert!(mountpoint.exists());
    }

    async fn full_check<P: Deref<Target = Plugin>>(
        plugin: &P,
        mountpoint: Option<PathBuf>,
        status: VolumeStatus,
    ) {
        plugin
            .test_in_list(vec![ItemVolume {
                name: VOLUME_NAME.into(),
                mountpoint: mountpoint.clone(),
            }])
            .await
            .test_stub_path_is(mountpoint.clone())
            .await
            .test_get_stub_volume(VolumeInfo {
                mountpoint: mountpoint.clone(),
                status: status.into(),
            })
            .await;
    }

    #[tokio::test]
    async fn happy_flow_create_mount_get_path_unmount_remove() {
        let (_g, plugin) = Plugin::temp().await.with_stub_test_repo().await;
        full_check(&plugin, None, VolumeStatus::Created).await;

        let mountpoint = plugin.mount(VOLUME_NAME, "id").await.unwrap();
        full_check(&plugin, Some(mountpoint.clone()), VolumeStatus::Clonned).await;
        assert!(mountpoint.exists());

        plugin.unmount(VOLUME_NAME, "id").await.unwrap();
        full_check(&plugin, None, VolumeStatus::Cleared).await;
        assert!(!mountpoint.exists());

        plugin.remove(VOLUME_NAME).await.unwrap();
        plugin
            .test_is_empty_list()
            .await
            .test_stub_path_is(None)
            .await;
    }
}
