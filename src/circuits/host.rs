use super::Limb;
use crate::utils::GateCell;
use halo2_proofs::pairing::bn256::Fr;
use std::marker::PhantomData;

use crate::{
    customized_circuits, customized_circuits_expand, item_count, table_item, value_for_assign,
};

use halo2_proofs::{
    arithmetic::FieldExt,
    circuit::{Chip, Layouter, Region},
    plonk::{Advice, Column, ConstraintSystem, Error, Expression, Fixed, Selector, VirtualCells},
    poly::Rotation,
};

use crate::constant_from;

#[rustfmt::skip]
customized_circuits!(HostOpConfig, 2, 8, 3, 0,
    | shared_operand | shared_opcode | shared_index   | filtered_opcode  | filtered_index | enable   | filtered_operand   | merged_op   | indicator | p1 | sel
    | nil            | nil           | shared_index_n | nil | nil            | enable_n | filtered_operand_n | merged_op_n | nil       | nil              | nil
);

impl HostOpConfig {
    pub fn configure<F: FieldExt>(&self, cs: &mut ConstraintSystem<F>, _opcodes: &Vec<Fr>) {
        cs.lookup_any("filter-shared-ops", |meta| {
            let sopc = self.get_expr(meta, HostOpConfig::shared_opcode());
            let soper = self.get_expr(meta, HostOpConfig::shared_operand());
            let sidx = self.get_expr(meta, HostOpConfig::shared_index());
            let enable = self.get_expr(meta, HostOpConfig::enable());
            let fopc = self.get_expr(meta, HostOpConfig::filtered_opcode());
            let foper = self.get_expr(meta, HostOpConfig::filtered_operand());
            let fidx = self.get_expr(meta, HostOpConfig::filtered_index());
            let sel = self.get_expr(meta, HostOpConfig::sel());
            let enable = enable * sel;
            vec![
                (fidx * enable.clone(), sidx),
                (foper * enable.clone(), soper),
                (fopc * enable.clone(), sopc),
            ]
        });

        cs.create_gate("merge operands in filtered columns", |meta| {
            let merged_op = self.get_expr(meta, HostOpConfig::merged_op());
            let merged_op_n = self.get_expr(meta, HostOpConfig::merged_op_n());
            let cur_op = self.get_expr(meta, HostOpConfig::filtered_operand());
            let indicator = self.get_expr(meta, HostOpConfig::indicator());
            vec![indicator.clone() * (merged_op - (merged_op_n * indicator + cur_op))]
        });

        /* enable is continuous with pattern 1,1,1,1,1,0,0,0,0 when sel is active */
        cs.create_gate("enable consistant", |meta| {
            let enable = self.get_expr(meta, HostOpConfig::enable());
            let enable_n = self.get_expr(meta, HostOpConfig::enable_n());
            let sel = self.get_expr(meta, HostOpConfig::sel());
            vec![(enable - constant_from!(1 as u64)) * enable_n * sel]
        });
    }

    pub fn assign_merged_operands(
        &self,
        region: &mut Region<Fr>,
        offset: &mut usize,
        values: Vec<&((Fr, Fr), Fr)>,
        indicator: Fr,
        enable: bool,
    ) -> Result<(Limb<Fr>, Limb<Fr>), Error> {
        let mut rev = values.clone();
        let len = values.len();
        rev.reverse();
        let mut merged_ops = vec![];
        let mut merged_acc = Fr::zero();
        for c in rev.iter() {
            merged_acc = c.0 .0 + merged_acc * indicator;
            merged_ops.push(merged_acc);
        }
        merged_ops.reverse();
        let mut ret = None;
        let mut op = None;
        for (i, (((operand, opcode), index), merged_op)) in
            values.into_iter().zip(merged_ops).enumerate()
        {
            self.assign_cell(region, *offset, &HostOpConfig::filtered_operand(), *operand)?;
            let opc =
                self.assign_cell(region, *offset, &HostOpConfig::filtered_opcode(), *opcode)?;
            self.assign_cell(region, *offset, &HostOpConfig::filtered_index(), *index)?;
            self.assign_cell(
                region,
                *offset,
                &HostOpConfig::enable(),
                Fr::from(enable as u64),
            )?;
            self.assign_cell(region, *offset, &HostOpConfig::sel(), Fr::one())?;
            let limb = self.assign_cell(region, *offset, &HostOpConfig::merged_op(), merged_op)?;
            if i == len - 1 {
                self.assign_cell(region, *offset, &HostOpConfig::indicator(), Fr::zero())?;
            } else {
                self.assign_cell(region, *offset, &HostOpConfig::indicator(), indicator)?;
                if i == 0 {
                    ret = Some(limb);
                    op = Some(opc);
                }
            }
            *offset += 1;
        }
        Ok((ret.unwrap(), op.unwrap()))
    }

    pub fn assign_one_line(
        &self,
        region: &mut Region<Fr>,
        offset: &mut usize,
        operand: Fr,
        opcode: Fr,
        index: Fr,
        merge: Fr,
        ind: Fr,
        enable: bool,
    ) -> Result<(Limb<Fr>, Limb<Fr>), Error> {
        let r = self.assign_cell(region, *offset, &HostOpConfig::filtered_operand(), operand)?;
        let op = self.assign_cell(region, *offset, &HostOpConfig::filtered_opcode(), opcode)?;
        self.assign_cell(region, *offset, &HostOpConfig::filtered_index(), index)?;
        self.assign_cell(region, *offset, &HostOpConfig::indicator(), ind)?;
        self.assign_cell(region, *offset, &HostOpConfig::merged_op(), merge)?;
        self.assign_cell(
            region,
            *offset,
            &HostOpConfig::enable(),
            Fr::from(enable as u64),
        )?;
        self.assign_cell(region, *offset, &HostOpConfig::sel(), Fr::one())?;
        *offset += 1;
        Ok((r, op))
    }
}

pub trait HostOpSelector {
    type Config: Clone + std::fmt::Debug;
    fn configure(meta: &mut ConstraintSystem<Fr>, shared_advice: &Vec<Column<Advice>>) -> Self::Config;
    fn construct(c: Self::Config) -> Self;
    fn opcodes() -> Vec<Fr>;
    fn assign(
        region: &mut Region<Fr>,
        offset: &mut usize,
        shared_operands: &Vec<Fr>,
        shared_opcodes: &Vec<Fr>,
        config: &HostOpConfig,
    ) -> Result<Vec<Limb<Fr>>, Error>;
    fn synthesize(
        &mut self,
        offset: &mut usize,
        arg_cells: &Vec<Limb<Fr>>,
        layouter: &mut impl Layouter<Fr>,
    ) -> Result<(), Error>;
}

pub struct HostOpChip<F: FieldExt, S: HostOpSelector> {
    pub config: HostOpConfig,
    pub selector_chip_config: S::Config,
    _marker: PhantomData<(F, S)>,
}

impl<F: FieldExt, S: HostOpSelector> Chip<F> for HostOpChip<F, S> {
    type Config = HostOpConfig;
    type Loaded = ();

    fn config(&self) -> &Self::Config {
        &self.config
    }

    fn loaded(&self) -> &Self::Loaded {
        &()
    }
}

impl<S: HostOpSelector> HostOpChip<Fr, S> {
    pub fn construct(config: <Self as Chip<Fr>>::Config, selector_chip_config: S::Config) -> Self {
        Self {
            config,
            selector_chip_config,
            _marker: PhantomData,
        }
    }

    pub fn configure(cs: &mut ConstraintSystem<Fr>, shared_advices: &Vec<Column<Advice>>) -> <Self as Chip<Fr>>::Config {
        let witness = [
                cs.named_advice_column("shared_operands".to_string()),
                cs.named_advice_column("shared_opcodes".to_string()),
                cs.named_advice_column("shared_index".to_string()),
                shared_advices[0].clone(),
                shared_advices[1].clone(),
                shared_advices[2].clone(),
                shared_advices[3].clone(),
                shared_advices[4].clone(),
        ];
        witness.map(|x| cs.enable_equality(x));
        let fixed = [
            cs.fixed_column(),
            cs.fixed_column(),
            cs.fixed_column(),
        ];
        fixed.map(|x| cs.enable_equality(x));
        let selector = [];

        let config = HostOpConfig::new(witness, fixed, selector);
        config.configure(cs, &S::opcodes());
        config
    }

    pub fn assign(
        &self,
        layouter: &mut impl Layouter<Fr>,
        arg_offset: &mut usize,
        shared_operands: &Vec<Fr>,
        shared_opcodes: &Vec<Fr>,
    ) -> Result<Vec<Limb<Fr>>, Error> {
        let mut arg_cells = None;
        *arg_offset = layouter.assign_region(
            || "filter operands and opcodes",
            |mut region| {
                println!("assign_region");
                let mut offset = 0;
                let mut index = 0;
                self.config.assign_cell(
                    &mut region,
                    offset,
                    &HostOpConfig::shared_opcode(),
                    Fr::zero(),
                )?;
                self.config.assign_cell(
                    &mut region,
                    offset,
                    &HostOpConfig::shared_operand(),
                    Fr::zero(),
                )?;
                self.config.assign_cell(
                    &mut region,
                    offset,
                    &HostOpConfig::shared_index(),
                    Fr::zero(),
                )?;
                offset += 1;
                for opcode in shared_opcodes {
                    if S::opcodes().contains(&opcode) {
                        index += 1;
                     }
                    self.config.assign_cell(
                        &mut region,
                        offset,
                        &HostOpConfig::shared_opcode(),
                        opcode.clone(),
                    )?;
                    self.config.assign_cell(
                        &mut region,
                        offset,
                        &HostOpConfig::shared_operand(),
                        shared_operands[offset-1],
                    )?;
                    self.config.assign_cell(
                        &mut region,
                        offset,
                        &HostOpConfig::shared_index(),
                        Fr::from(index as u64),
                    )?;
                    offset += 1;
                }
                let mut local_offset = *arg_offset; 
                arg_cells = Some(S::assign(
                    &mut region,
                    &mut local_offset,
                    shared_operands,
                    shared_opcodes,
                    &self.config,
                )?);
                Ok(local_offset)
            },
        )?;
        Ok(arg_cells.unwrap())
    }
}
