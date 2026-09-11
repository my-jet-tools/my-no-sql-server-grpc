use my_no_sql_grpc_core::MyNoSqlEntity;

use crate::my_no_sql_writer_grpc::EntitySchemaGrpcModel;

/// The schema as it travels with every write.
///
/// The entity built it once, on the first ask, and this only puts it into the
/// shape the contract carries. The id is not computed here or anywhere else at
/// run time - it is a constant of the entity's type.
pub fn build_schema<TEntity: MyNoSqlEntity>() -> EntitySchemaGrpcModel {
    let schema = TEntity::get_schema();

    EntitySchemaGrpcModel {
        schema_id: schema.id,
        schema: schema.schema.clone(),
    }
}
