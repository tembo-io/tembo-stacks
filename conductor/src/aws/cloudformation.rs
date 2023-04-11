use aws_config::SdkConfig;
use aws_sdk_cloudformation::{config::Region, Client, Error};
use std::sync::Arc;

use crate::aws::cloudformation_template::create_template;

pub struct CloudFormationParams {
    pub s3_bucket_name: String,
    pub s3_bucket_path: String,
    pub iam_role_name: String,
    pub lifecycle_duration: Option<u32>,
}

pub struct AWSConfigState {
    pub cf_client: Arc<Client>,
    pub cf_config: Arc<SdkConfig>,
}

impl CloudFormationParams {
    pub fn new(
        s3_bucket_name: String,
        s3_bucket_path: String,
        iam_role_name: String,
        lifecycle_duration: Option<u32>,
    ) -> Self {
        Self {
            s3_bucket_name,
            s3_bucket_path,
            iam_role_name,
            lifecycle_duration,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.s3_bucket_name.is_empty() {
            return Err("S3 bucket name cannot be empty".to_string());
        }
        if self.s3_bucket_path.is_empty() {
            return Err("S3 bucket path cannot be empty".to_string());
        }
        if self.iam_role_name.is_empty() {
            return Err("IAM role name cannot be empty".to_string());
        }
        if let Some(duration) = self.lifecycle_duration {
            if duration == 0 {
                return Err("Lifecycle duration cannot be 0".to_string());
            }
        }
        Ok(())
    }
}

impl AWSConfigState {
    pub async fn new(region: Region) -> Self {
        let cf_config = Arc::new(aws_config::from_env().region(region).load().await);
        let cf_client = Arc::new(Client::new(&cf_config));
        Self {
            cf_client,
            cf_config,
        }
    }

    pub async fn does_stack_exist(&self, stack_name: &str) -> Result<bool, Error> {
        let describe_stacks_result = self
            .cf_client
            .describe_stacks()
            .stack_name(stack_name)
            .send()
            .await?;

        Ok(describe_stacks_result.stacks.is_some())
    }

    pub async fn create_cloudformation_stack(
        &self,
        stack_name: &str,
        params: &CloudFormationParams,
    ) -> Result<(), Error> {
        if !self.does_stack_exist(stack_name).await? {
            let template = create_template(params).await;
            // todo(nhudson): We need to add tags to the stack
            // get with @sjmiller609 to figure out how we want
            // to tag these CF stacks.
            let create_stack_result = self
                .cf_client
                .create_stack()
                .stack_name(stack_name)
                .template_body(template.to_string())
                .send()
                .await?;

            println!("Created stack: {:?}", create_stack_result.stack_id);
        } else {
            println!("Stack {:?} already exists, skipping creation", stack_name);
        }

        Ok(())
    }

    pub async fn delete_cloudformation_stack(&self, stack_name: &str) -> Result<(), Error> {
        if self.does_stack_exist(stack_name).await? {
            let delete_stack_result = self
                .cf_client
                .delete_stack()
                .stack_name(stack_name)
                .send()
                .await?;

            println!(
                "Deleted stack: {}, delete_stack_result: {:?}",
                stack_name, delete_stack_result
            );
        } else {
            println!("Stack {:?} does not exist, skipping deletion", stack_name);
        }

        Ok(())
    }
}
