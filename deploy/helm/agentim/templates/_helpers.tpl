{{- define "agentim.name" -}}{{ .Release.Name }}{{- end -}}
{{- define "agentim.labels" -}}
app.kubernetes.io/name: agentim
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}
