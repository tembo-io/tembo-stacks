kubectl delete deployment -n reconciler reconciler
kubectl delete clusterrolebinding -n reconciler reconciler-binding
kubectl delete clusterrole reconciler
kubectl delete servicemonitor -n reconciler reconciler-coredbs
kubectl delete serviceaccount -n reconciler reconciler
helm upgrade \
  --install \
  --set secret.postgresConnectionString='queue-conn' \
  --set dataPlaneEventsQueue=data_plane_events \
  --set controlPlaneEventsQueue=saas_queue \
  --namespace reconciler \
  reconciler \
  charts/reconciler
