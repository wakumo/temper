pipeline {
    environment {
    imagename = "temper"
    ecrurl = "http://10.123.31.221:5000/v2/"
    dockerImage = ''
    }
    agent any
    stages {
      
      stage('Cloning Git') {
        // when {branch 'main'}
        steps {
          git branch: 'main', credentialsId: 'github-develop', url: 'https://github.com/wakumo/temper.git'
        }
      }
      stage('Building image') {
        // when {branch 'main'}
        steps{
          script {
            dockerImage = docker.build imagename
          }
        }
      }
      stage('Push Image') {
        // when {branch 'main'}
        steps{
          script {
            // docker.withRegistry(ecrurl, ecrcredentials ) {
            docker.withRegistry(ecrurl) {
              dockerImage.push("$BUILD_NUMBER")
              dockerImage.push('latest')
            }
          }
        }
      }
      stage('Kubernetes') {
        // when {branch 'main'}
        steps{
          script{
            withKubeConfig(credentialsId: 'wkm_local_credential_deploy', namespace: 'ethereum-tx', serverUrl: 'https://10.123.31.100:6443') {
              sh 'curl -LO "https://storage.googleapis.com/kubernetes-release/release/v1.20.5/bin/linux/amd64/kubectl"'  
              sh 'chmod u+x ./kubectl'  
              
              sh './kubectl apply -f .kube/development/temper-deployment.yml'
              sh './kubectl rollout restart deployment/temper-deployment'
            }
          }
        }
      }
    }
    post {
       // only triggered when blue or green sign
       success {
           slackSend channel: 'ethereum-tx-jenkins-notification-dev', message: "[temper] git-commit {${GIT_COMMIT}} has been deployed!!!", color: '#1ddb46'
       }
       // triggered when red sign
       failure {
           slackSend channel: 'ethereum-tx-jenkins-notification-dev', message: "[temper] something went wrong at git-commit {${GIT_COMMIT}}. please try again!!!", color: '#FE2E2E'
       }
       // trigger every-works
       always {
           sh "docker system prune -f"
       }
    }
  }