use std::fmt::Display;
use std::str::FromStr;

use itertools::Itertools;
use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::digit1;
use nom::combinator::{all_consuming, map, map_res, opt};
use nom::sequence::{delimited, preceded, separated_pair};
use nom::Parser;

use crate::arch::Arch;
use crate::device::{CoreRange, DeviceFile, DeviceMode};
use crate::{DeviceError, DeviceResult};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Count {
    Finite(u8),
    All,
}

impl Display for Count {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Count::Finite(c) => write!(f, "{c}"),
            // TODO: revise syntax below and bring according implementation to Config's FromStr
            Count::All => write!(f, "all"),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DeviceConfigInner {
    pub(crate) cfgs: Vec<Config>,
}

impl FromStr for DeviceConfigInner {
    type Err = DeviceError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            cfgs: s
                .split(',')
                .map(Config::from_str)
                .collect::<Result<Vec<_>, Self::Err>>()?,
        })
    }
}

impl Display for DeviceConfigInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.cfgs.iter().map(|c| c.to_string()).join(","))
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Config {
    // TODO: Named cannot describe MultiCore yet.
    Named {
        device_id: u8,
        core_range: CoreRange,
    },
    Unnamed {
        arch: Arch,
        core_num: u8,
        mode: DeviceMode,
        count: Count,
    },
}

impl Config {
    pub(crate) fn fit(&self, arch: Arch, device_file: &DeviceFile) -> bool {
        match self {
            Self::Named {
                device_id,
                core_range,
            } => {
                device_file.device_index() == *device_id && device_file.core_range() == *core_range
            }
            Self::Unnamed {
                arch: config_arch,
                mode,
                ..
            } => arch == *config_arch && device_file.mode() == *mode,
        }
    }

    pub(crate) fn count(&self) -> Count {
        match self {
            Self::Named { .. } => Count::Finite(1),
            Self::Unnamed { count, .. } => *count,
        }
    }
}

impl FromStr for Config {
    type Err = DeviceError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        fn digit_to_u8<'a>(
        ) -> impl FnMut(&'a str) -> nom::IResult<&'a str, u8, nom::error::Error<&'a str>> {
            map_res(digit1, |s: &str| s.parse::<u8>())
        }

        fn parse_cores<'a>(
        ) -> impl FnMut(&'a str) -> nom::IResult<&'a str, CoreRange, nom::error::Error<&'a str>>
        {
            alt((
                map_res(
                    separated_pair(digit_to_u8(), tag("-"), digit_to_u8()),
                    CoreRange::try_from,
                ),
                map(digit_to_u8(), CoreRange::from),
            ))
        }

        // Try parsing a "npu0pe0" pattern. Note that "npu0" is also valid, which represents npu0 as MultiCore mode.
        fn legacy_parser(s: &str) -> DeviceResult<Config> {
            let parser_id = preceded(tag("npu"), digit_to_u8());
            let parser_cores = map(opt(preceded(tag("pe"), parse_cores())), |c| {
                c.unwrap_or(CoreRange::All)
            });

            let (_, (device_id, core_range)) = all_consuming(parser_id.and(parser_cores))(s)
                .map_err(|e| DeviceError::parse_error(s, e.to_string()))?;

            Ok(Config::Named {
                device_id,
                core_range,
            })
        }

        // Try parsing a "0:0" or "0:0-1" pattern. Note that "0" is also valid, which represents npu0 as MultiCore mode.
        fn named_cfg_parser(s: &str) -> DeviceResult<Config> {
            let parser_id = preceded(tag("npu:"), digit_to_u8());
            let parser_cores = map(opt(preceded(tag(":"), parse_cores())), |c| {
                c.unwrap_or(CoreRange::All)
            });

            let (_, (device_id, core_range)) = all_consuming(parser_id.and(parser_cores))(s)
                .map_err(|e| DeviceError::parse_error(s, e.to_string()))?;

            Ok(Config::Named {
                device_id,
                core_range,
            })
        }

        // Try parsing a "warboy(1)*1" pattern
        fn unnamed_cfg_parser(s: &str) -> DeviceResult<Config> {
            // Currently supports "warboy" only
            let parser_arch = map_res(tag("warboy"), |s: &str| s.parse::<Arch>());
            let parser_mode =
                map(
                    opt(delimited(tag("("), digit_to_u8(), tag(")"))),
                    |mode| match mode {
                        // "warboy" is equivalent to "warboy(1)"
                        None | Some(1) => (1, DeviceMode::Single),
                        // TODO: Improve below
                        Some(n) => (n, DeviceMode::Fusion),
                    },
                );
            let parser_count = preceded(tag("*"), digit_to_u8());

            // Note: nom::sequence::tuple requires parsers to have equivalent signatures
            let (_, ((arch, (core_num, mode)), count)) =
                all_consuming(parser_arch.and(parser_mode).and(parser_count))(s)
                    .map_err(|e| DeviceError::parse_error(s, e.to_string()))?;

            Ok(Config::Unnamed {
                arch,
                core_num,
                mode,
                count: Count::Finite(count),
            })
        }

        legacy_parser(s)
            .or_else(|_| named_cfg_parser(s))
            .or_else(|_| unnamed_cfg_parser(s))
    }
}

impl Display for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Named {
                device_id,
                core_range,
            } => match core_range {
                CoreRange::All => {
                    write!(f, "npu:{device_id}")
                }
                CoreRange::Range((s, e)) => {
                    if s == e {
                        write!(f, "npu:{device_id}:{s}")
                    } else {
                        write!(f, "npu:{device_id}:{s}-{e}")
                    }
                }
            },
            Self::Unnamed {
                arch,
                core_num,
                mode: _mode,
                count,
            } => {
                if *core_num == 0 {
                    write!(f, "{arch}*{count}")
                } else {
                    write!(f, "{arch}({core_num})*{count}")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeviceResult;

    #[test]
    fn test_multiple_configs_repr() -> eyre::Result<()> {
        let repr = "npu:0:0,npu:0:1";
        let config = repr.parse::<DeviceConfigInner>()?;

        assert_eq!(repr, config.to_string().as_str());

        Ok(())
    }

    #[tokio::test]
    async fn test_named_config_fit() -> DeviceResult<()> {
        let config = "npu:0:0".parse::<Config>().unwrap();
        let npu0pe0 = crate::get_device_file_with("../test_data/test-0/dev", "npu0pe0").await?;
        let npu0pe1 = crate::get_device_file_with("../test_data/test-0/dev", "npu0pe1").await?;
        let npu0pe0_1 = crate::get_device_file_with("../test_data/test-0/dev", "npu0pe0-1").await?;
        let npu1pe0 = crate::get_device_file_with("../test_data/test-0/dev", "npu0pe1").await?;

        assert_eq!(config.count(), Count::Finite(1));

        assert!(config.fit(Arch::WarboyB0, &npu0pe0));
        assert!(!config.fit(Arch::WarboyB0, &npu0pe1));
        assert!(!config.fit(Arch::WarboyB0, &npu0pe0_1));
        assert!(!config.fit(Arch::WarboyB0, &npu1pe0));

        Ok(())
    }

    #[tokio::test]
    async fn test_unnamed_config_fit() -> DeviceResult<()> {
        let config = "warboy(1)*2".parse::<Config>().unwrap();

        assert_eq!(config.count(), Count::Finite(2));

        let npu0pe0 = crate::get_device_file_with("../test_data/test-0/dev", "npu0pe0").await?;
        let npu0pe1 = crate::get_device_file_with("../test_data/test-0/dev", "npu0pe1").await?;
        let npu0pe0_1 = crate::get_device_file_with("../test_data/test-0/dev", "npu0pe0-1").await?;

        assert!(config.fit(Arch::WarboyB0, &npu0pe0));
        assert!(config.fit(Arch::WarboyB0, &npu0pe1));
        assert!(!config.fit(Arch::Renegade, &npu0pe0));
        assert!(!config.fit(Arch::WarboyB0, &npu0pe0_1));

        Ok(())
    }

    #[test]
    fn test_config_from_named_text_repr() -> eyre::Result<()> {
        assert!("0".parse::<Config>().is_err());
        assert!("npu0:".parse::<Config>().is_err());
        assert!("npu:0:0-1-".parse::<Config>().is_err());
        assert!("npu:0:1-0".parse::<Config>().is_err());

        assert_eq!(
            "npu:0".parse::<Config>()?,
            Config::Named {
                device_id: 0,
                core_range: CoreRange::All
            }
        );
        assert_eq!(
            "npu:1".parse::<Config>()?,
            Config::Named {
                device_id: 1,
                core_range: CoreRange::All
            }
        );
        assert_eq!(
            "npu:0:0".parse::<Config>()?,
            Config::Named {
                device_id: 0,
                core_range: CoreRange::Range((0, 0))
            }
        );
        assert_eq!(
            "npu:0:1".parse::<Config>()?,
            Config::Named {
                device_id: 0,
                core_range: CoreRange::Range((1, 1))
            }
        );
        assert_eq!(
            "npu:1:1".parse::<Config>()?,
            Config::Named {
                device_id: 1,
                core_range: CoreRange::Range((1, 1))
            }
        );
        assert_eq!(
            "npu:0:0-1".parse::<Config>()?,
            Config::Named {
                device_id: 0,
                core_range: CoreRange::Range((0, 1))
            }
        );

        Ok(())
    }

    #[test]
    fn test_config_from_unnamed_text_repr() -> eyre::Result<()> {
        assert!("warboy".parse::<Config>().is_err());
        assert!("warboy*".parse::<Config>().is_err());
        assert!("*1".parse::<Config>().is_err());
        assert!("some_npu*10".parse::<Config>().is_err());
        assert!("warboy(2*10".parse::<Config>().is_err());
        assert_eq!(
            "warboy(1)*2".parse::<Config>()?,
            Config::Unnamed {
                arch: Arch::WarboyB0,
                core_num: 1,
                mode: DeviceMode::Single,
                count: Count::Finite(2),
            }
        );
        assert_eq!(
            "warboy(2)*4".parse::<Config>()?,
            Config::Unnamed {
                arch: Arch::WarboyB0,
                core_num: 2,
                mode: DeviceMode::Fusion,
                count: Count::Finite(4)
            }
        );
        assert_eq!(
            "warboy*12".parse::<Config>()?,
            Config::Unnamed {
                arch: Arch::WarboyB0,
                core_num: 1,
                mode: DeviceMode::Single,
                count: Count::Finite(12)
            }
        );
        // assert!("npu*10".parse::<Config>().is_ok());

        Ok(())
    }

    #[test]
    fn test_from_legacy_text_repr() -> eyre::Result<()> {
        assert!("npu0pe".parse::<Config>().is_err());
        assert!("npupe0".parse::<Config>().is_err());
        assert!("npu0pe0,".parse::<Config>().is_err());

        assert_eq!(
            "npu1pe1".parse::<Config>()?,
            Config::Named {
                device_id: 1,
                core_range: CoreRange::Range((1, 1))
            }
        );

        assert_eq!(
            "npu1pe0-1".parse::<Config>()?,
            Config::Named {
                device_id: 1,
                core_range: CoreRange::Range((0, 1))
            }
        );

        assert_eq!(
            "npu1".parse::<Config>()?,
            Config::Named {
                device_id: 1,
                core_range: CoreRange::All
            }
        );

        Ok(())
    }
}
