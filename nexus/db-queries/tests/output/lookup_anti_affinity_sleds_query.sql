WITH
  our_groups
    AS (SELECT group_id FROM anti_affinity_group_instance_membership WHERE instance_id = $1),
  other_instances
    AS (
      SELECT
        anti_affinity_group_instance_membership.group_id, instance_id
      FROM
        anti_affinity_group_instance_membership
        JOIN our_groups ON anti_affinity_group_instance_membership.group_id = our_groups.group_id
      WHERE
        instance_id != $2
    ),
  other_instances_by_policy
    AS (
      SELECT
        policy, instance_id
      FROM
        other_instances
        JOIN anti_affinity_group ON
            anti_affinity_group.id = other_instances.group_id
            AND anti_affinity_group.failure_domain = 'sled'
      WHERE
        anti_affinity_group.time_deleted IS NULL
    )
SELECT
  DISTINCT policy, sled_id
FROM
  other_instances_by_policy
  JOIN sled_resource_vmm ON sled_resource_vmm.instance_id = other_instances_by_policy.instance_id
