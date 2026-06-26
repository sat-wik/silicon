//! M1: parses a CMSIS-SVD file into a queryable peripheral/register/field model.
//!
//! Ground-truth source only — this crate never decides whether firmware is
//! correct, it just answers "what does the SVD say about X". See CLAUDE.md
//! invariant 1: evidence comes from the SVD, never from an LLM, and the
//! checker (crate `checker`) is where violations get decided.

use std::collections::HashMap;

pub use svd_parser::svd::Access;

/// A parsed device: every peripheral, register and field the SVD describes.
#[derive(Debug, Clone)]
pub struct Model {
    peripherals: Vec<PeripheralModel>,
    index: HashMap<String, usize>,
}

#[derive(Debug, Clone)]
pub struct PeripheralModel {
    pub name: String,
    pub base_address: u64,
    pub registers: Vec<RegisterModel>,
}

#[derive(Debug, Clone)]
pub struct RegisterModel {
    pub name: String,
    pub address_offset: u32,
    pub size_bits: u32,
    pub access: Access,
    pub reset_value: Option<u64>,
    pub fields: Vec<FieldModel>,
}

#[derive(Debug, Clone)]
pub struct FieldModel {
    pub name: String,
    pub bit_offset: u32,
    pub bit_width: u32,
    pub access: Access,
    pub reset_value: Option<u64>,
    /// `None` means the SVD has no enumeration for this field: value-membership
    /// is unverifiable, not "anything goes". Callers must not treat this as
    /// permissive — see CLAUDE.md invariant 6.
    pub allowed_values: Option<Vec<EnumValue>>,
}

#[derive(Debug, Clone)]
pub struct EnumValue {
    pub name: String,
    pub value: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("failed to parse SVD")]
    Parse(#[source] anyhow::Error),
}

impl Model {
    /// Parse a CMSIS-SVD XML document into a queryable model.
    pub fn from_svd_str(xml: &str) -> Result<Model, ModelError> {
        let config = svd_parser::Config::default()
            .expand(true)
            .expand_properties(true);
        let device = svd_parser::parse_with_config(xml, &config).map_err(ModelError::Parse)?;
        Ok(Model::from_device(&device))
    }

    pub fn peripheral(&self, name: &str) -> Option<&PeripheralModel> {
        self.index.get(name).map(|&i| &self.peripherals[i])
    }

    pub fn register(&self, peripheral: &str, register: &str) -> Option<&RegisterModel> {
        self.peripheral(peripheral)?
            .registers
            .iter()
            .find(|r| r.name == register)
    }

    pub fn field(&self, peripheral: &str, register: &str, field: &str) -> Option<&FieldModel> {
        self.register(peripheral, register)?
            .fields
            .iter()
            .find(|f| f.name == field)
    }

    pub fn peripherals(&self) -> &[PeripheralModel] {
        &self.peripherals
    }

    fn from_device(device: &svd_parser::svd::Device) -> Model {
        let peripherals: Vec<PeripheralModel> = device
            .peripherals
            .iter()
            .map(PeripheralModel::from_svd)
            .collect();
        let index = peripherals
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.clone(), i))
            .collect();
        Model { peripherals, index }
    }
}

impl PeripheralModel {
    fn from_svd(p: &svd_parser::svd::Peripheral) -> PeripheralModel {
        let mut registers = Vec::new();
        if let Some(children) = &p.registers {
            for rc in children {
                collect_registers(rc, 0, &mut registers);
            }
        }
        PeripheralModel {
            name: p.name.clone(),
            base_address: p.base_address,
            registers,
        }
    }
}

fn collect_registers(
    rc: &svd_parser::svd::RegisterCluster,
    base_offset: u32,
    out: &mut Vec<RegisterModel>,
) {
    use svd_parser::svd::RegisterCluster;
    match rc {
        RegisterCluster::Register(r) => out.push(RegisterModel::from_svd(r, base_offset)),
        RegisterCluster::Cluster(c) => {
            let cluster_offset = base_offset + c.address_offset;
            for child in &c.children {
                collect_registers(child, cluster_offset, out);
            }
        }
    }
}

impl RegisterModel {
    fn from_svd(r: &svd_parser::svd::Register, base_offset: u32) -> RegisterModel {
        let access = r.properties.access.unwrap_or(Access::ReadWrite);
        let reset_value = r.properties.reset_value;
        let fields = r
            .fields
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|f| FieldModel::from_svd(f, access, reset_value))
            .collect();
        RegisterModel {
            name: r.name.clone(),
            address_offset: base_offset + r.address_offset,
            size_bits: r.properties.size.unwrap_or(32),
            access,
            reset_value,
            fields,
        }
    }
}

impl FieldModel {
    fn from_svd(
        f: &svd_parser::svd::Field,
        register_access: Access,
        register_reset_value: Option<u64>,
    ) -> FieldModel {
        let bit_offset = f.bit_range.offset;
        let bit_width = f.bit_range.width;
        let access = f.access.unwrap_or(register_access);
        let reset_value = register_reset_value.map(|rv| {
            let mask = if bit_width >= 64 {
                u64::MAX
            } else {
                (1u64 << bit_width) - 1
            };
            (rv >> bit_offset) & mask
        });
        let allowed_values = write_enum_values(f);
        FieldModel {
            name: f.name.clone(),
            bit_offset,
            bit_width,
            access,
            reset_value,
            allowed_values,
        }
    }
}

/// The enum set applicable to a *write* of this field, if the SVD defines one.
/// A field may carry separate read/write enumerations (CMSIS-SVD `usage`);
/// a read-only enum says nothing about which values are valid to write.
fn write_enum_values(f: &svd_parser::svd::Field) -> Option<Vec<EnumValue>> {
    use svd_parser::svd::Usage;
    let set = f.enumerated_values.iter().find(|ev| {
        !matches!(ev.usage, Some(Usage::Read))
    })?;
    let values: Vec<EnumValue> = set
        .values
        .iter()
        .filter_map(|v| {
            v.value.map(|value| EnumValue {
                name: v.name.clone(),
                value,
            })
        })
        .collect();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp2040_model() -> Model {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/rp2040.svd");
        let xml = std::fs::read_to_string(path).expect("vendored rp2040.svd must exist");
        Model::from_svd_str(&xml).expect("rp2040.svd must parse")
    }

    #[test]
    fn sio_gpio_oe_has_no_enum_and_is_unverifiable() {
        let model = rp2040_model();
        let field = model
            .field("SIO", "GPIO_OE", "GPIO_OE")
            .expect("SIO.GPIO_OE.GPIO_OE must exist in the RP2040 SVD");
        assert_eq!(field.bit_offset, 0);
        assert_eq!(field.bit_width, 30);
        assert_eq!(field.access, Access::ReadWrite);
        assert_eq!(field.reset_value, Some(0));
        assert!(
            field.allowed_values.is_none(),
            "GPIO_OE has no SVD enum; membership must be reported unverifiable, not guessed"
        );
    }

    #[test]
    fn pll_sys_fbdiv_int_has_no_enum_and_is_unverifiable() {
        let model = rp2040_model();
        let field = model
            .field("PLL_SYS", "FBDIV_INT", "FBDIV_INT")
            .expect("PLL_SYS.FBDIV_INT.FBDIV_INT must exist in the RP2040 SVD");
        assert_eq!(field.bit_offset, 0);
        assert_eq!(field.bit_width, 12);
        assert_eq!(field.access, Access::ReadWrite);
        assert!(field.allowed_values.is_none());
    }

    #[test]
    fn clocks_gpout0_ctrl_auxsrc_has_enum_with_known_values() {
        let model = rp2040_model();
        let field = model
            .field("CLOCKS", "CLK_GPOUT0_CTRL", "AUXSRC")
            .expect("CLOCKS.CLK_GPOUT0_CTRL.AUXSRC must exist in the RP2040 SVD");
        assert_eq!(field.bit_offset, 5);
        assert_eq!(field.bit_width, 4);
        let allowed = field
            .allowed_values
            .as_ref()
            .expect("AUXSRC has an SVD enum and must be verifiable");
        assert_eq!(allowed.len(), 11);
        assert!(allowed.iter().any(|v| v.name == "clksrc_pll_sys" && v.value == 0));
        assert!(allowed.iter().any(|v| v.name == "clk_ref" && v.value == 10));

        // value 11 fits the 4-bit width but is not in the enum: not membership-valid.
        assert!(!allowed.iter().any(|v| v.value == 11));
        // value 16 doesn't even fit the 4-bit field width — a distinct violation class (M3).
        assert!(16 >= (1u64 << field.bit_width));
    }

    #[test]
    fn unknown_peripheral_register_field_resolve_to_none() {
        let model = rp2040_model();
        assert!(model.peripheral("NOT_A_PERIPHERAL").is_none());
        assert!(model.register("SIO", "NOT_A_REGISTER").is_none());
        assert!(model.field("SIO", "GPIO_OE", "NOT_A_FIELD").is_none());
    }
}
